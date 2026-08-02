#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import {
  lstatSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { request } from "node:http";
import { userInfo } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const argumentsList = process.argv.slice(2);
if (argumentsList.some((argument) => argument !== "--live")) {
  console.error("usage: scripts/audit-runtime-boundary.mjs [--live]");
  process.exit(64);
}
const live = argumentsList.includes("--live");

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function source(path) {
  return readFileSync(join(repo, path), "utf8");
}

function requireSource(path, fragments) {
  const text = source(path);
  for (const fragment of fragments) {
    assert(text.includes(fragment), `${path} is missing a runtime-boundary contract`);
  }
}

function mode(path) {
  return statSync(path).mode & 0o777;
}

function walkPrivate(root) {
  const pending = [root];
  while (pending.length > 0) {
    const path = pending.pop();
    const metadata = lstatSync(path);
    assert(!metadata.isSymbolicLink(), "runtime state must not contain symbolic links");
    if (metadata.isDirectory()) {
      assert(mode(path) === 0o700, "runtime state directories must use mode 0700");
      for (const entry of readdirSync(path)) pending.push(join(path, entry));
    } else if (metadata.isFile()) {
      assert(mode(path) === 0o600, "runtime state files must use mode 0600");
    } else {
      throw new Error("runtime state must contain only regular files and directories");
    }
  }
}

function httpRequest(method, path, { token, headers = {} } = {}) {
  return new Promise((resolveRequest, reject) => {
    const requestHeaders = { ...headers };
    if (token !== undefined) requestHeaders.authorization = `Bearer ${token}`;
    const req = request(
      {
        hostname: "127.0.0.1",
        port: 3334,
        method,
        path,
        headers: requestHeaders,
        agent: false,
        timeout: 2_000,
      },
      (response) => {
        const chunks = [];
        let receivedBytes = 0;
        response.on("error", reject);
        response.on("data", (chunk) => {
          receivedBytes += chunk.length;
          if (receivedBytes > 1024 * 1024) {
            response.destroy(new Error("local response exceeded one MiB"));
            return;
          }
          chunks.push(chunk);
        });
        response.on("end", () => {
          resolveRequest({
            status: response.statusCode,
            body: Buffer.concat(chunks),
            headers: response.headers,
          });
        });
      },
    );
    req.on("timeout", () => req.destroy(new Error("local request timed out")));
    req.on("error", reject);
    req.end();
  });
}

function parseRouterRoutes() {
  const daemonSource = source("apps/woof_d/src/lib.rs");
  const block = daemonSource.match(
    /pub fn router\(state: AppState\) -> Router \{[\s\S]*?Router::new\(\)([\s\S]*?)\.fallback\(/,
  );
  assert(block !== null, "could not locate the daemon router");

  const routes = [];
  const routePattern = /\.route\(\s*"([^"]+)"\s*,([\s\S]*?)\)\s*(?=\.route\(|\.fallback\()/g;
  for (const match of `${block[1]}.fallback(`.matchAll(routePattern)) {
    const path = match[1];
    const methods = [...match[2].matchAll(/\b(get|post|put|patch|delete)\s*\(/g)].map(
      (method) => method[1].toUpperCase(),
    );
    assert(methods.length > 0, `daemon route ${path} has no recognized method`);
    for (const method of methods) routes.push(`${method} ${path}`);
  }
  return [...new Set(routes)].sort();
}

function contractRoutes(path) {
  const contract = JSON.parse(source(path));
  assert(
    contract.public && typeof contract.public === "object",
    `${path} has no public route contract`,
  );
  assert(
    Array.isArray(contract.authenticated_routes),
    `${path} has no authenticated route ledger`,
  );
  return [...Object.keys(contract.public), ...contract.authenticated_routes].sort();
}

function assertExactRouteContracts() {
  const implementationRoutes = parseRouterRoutes();
  const contractPaths = [
    "docs/contracts/http.json",
    "docs/contracts/backend/http-routes.json",
  ];
  for (const path of contractPaths) {
    const routes = contractRoutes(path);
    assert(
      routes.length === new Set(routes).size,
      `${path} contains duplicate HTTP routes`,
    );
    assert(
      JSON.stringify(routes) === JSON.stringify(implementationRoutes),
      `${path} does not exactly match the daemon router`,
    );
  }

  const publicContract = JSON.parse(source("docs/contracts/http.json"));
  const backendContract = JSON.parse(source("docs/contracts/backend/http-routes.json"));
  const groupedRoutes = Object.values(backendContract.route_groups).flat().sort();
  assert(
    groupedRoutes.length === new Set(groupedRoutes).size,
    "backend HTTP route groups contain a duplicate route",
  );
  assert(
    JSON.stringify(groupedRoutes) ===
      JSON.stringify([...backendContract.authenticated_routes].sort()),
    "backend HTTP route groups do not cover the authenticated ledger exactly",
  );
  assert(
    JSON.stringify([...publicContract.mcp_read_routes].sort()) ===
      JSON.stringify([...backendContract.route_groups.mcp_read_only].sort()),
    "public and backend MCP route ledgers differ",
  );
  assert(
    backendContract.route_groups.mcp_read_only.every((route) => route.startsWith("GET ")),
    "an MCP route is not read-only",
  );

  const mcpSource = source("apps/woof-mcp/src/lib.rs");
  const toolRouteBlock = mcpSource.match(
    /fn tool_request\([\s\S]*?let path = match name \{([\s\S]*?)\n    \};\n    Ok\(\(path, query\)\)/,
  );
  assert(toolRouteBlock !== null, "could not locate MCP tool routing");
  const implementedMcpRoutes = [
    ...new Set([...toolRouteBlock[1].matchAll(/"(\/[^"?]+)"/g)].map((match) => match[1])),
  ].sort();
  const contractedMcpRoutes = backendContract.route_groups.mcp_read_only
    .map((route) => route.slice("GET ".length))
    .sort();
  assert(
    JSON.stringify(implementedMcpRoutes) === JSON.stringify(contractedMcpRoutes),
    "MCP bridge routes do not match the read-only HTTP ledger",
  );
  assert(
    mcpSource.includes(".client\n            .get(url)"),
    "MCP bridge does not issue GET requests",
  );

  const tools = JSON.parse(source("docs/contracts/backend/mcp-tools.json"));
  const expectedToolNames = [
    "get_chronicle",
    "get_recent_activity",
    "get_snapshots",
    "get_time_report",
    "get_wiki_page",
    "get_working_memory",
    "list_time_rules",
    "list_wiki",
    "search_memory",
    "search_wiki",
  ];
  assert(
    Array.isArray(tools) && tools.length === 10,
    "MCP contract must define exactly ten tools",
  );
  assert(
    JSON.stringify(tools.map((tool) => tool.name).sort()) === JSON.stringify(expectedToolNames),
    "MCP contract tool names changed",
  );
  assert(
    tools.every(
      (tool) =>
        tool.inputSchema?.type === "object" && tool.inputSchema.additionalProperties === false,
    ),
    "every MCP input schema must reject unknown arguments",
  );
}

function processRows() {
  return execFileSync("/bin/ps", ["-axo", "pid=,ppid=,command="], {
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
    .filter(Boolean)
    .map((match) => ({ pid: Number(match[1]), ppid: Number(match[2]), command: match[3] }));
}

function productionSources() {
  const roots = [
    "apps/woof/src",
    "apps/woof/src-tauri/src",
    "apps/woof-mcp/src",
    "apps/woof_d/src",
    ...readdirSync(join(repo, "crates"))
      .map((entry) => join("crates", entry, "src"))
      .filter((path) => {
        try {
          return statSync(join(repo, path)).isDirectory();
        } catch {
          return false;
        }
      }),
  ];
  const files = [];
  const visit = (relativePath) => {
    const absolutePath = join(repo, relativePath);
    const metadata = lstatSync(absolutePath);
    assert(!metadata.isSymbolicLink(), "production source inventory must not follow symbolic links");
    if (metadata.isDirectory()) {
      for (const entry of readdirSync(absolutePath).sort()) {
        visit(join(relativePath, entry));
      }
      return;
    }
    if (
      metadata.isFile() &&
      (relativePath.endsWith(".rs") ||
        relativePath.endsWith(".ts") ||
        relativePath.endsWith(".svelte"))
    ) {
      files.push(relativePath);
    }
  };
  for (const root of roots) visit(root);
  return files.sort();
}

function productionText(path) {
  const text = source(path);
  if (!path.endsWith(".rs")) return text;
  const testModule = text.search(/\n#\[cfg\(test\)\]\nmod tests\s*\{/u);
  return testModule === -1 ? text : text.slice(0, testModule);
}

function assertNetworkSourceInventory() {
  const expectedPrimitives = new Map([
    ["apps/woof-mcp/src/lib.rs", 1],
    ["apps/woof/src-tauri/src/commands.rs", 1],
    ["apps/woof/src-tauri/src/supervisor.rs", 1],
    ["apps/woof_d/src/main.rs", 2],
    ["crates/woof-llm/src/chat.rs", 1],
    ["crates/woof-llm/src/realtime.rs", 1],
  ]);
  const expectedDestinations = new Map([
    ["apps/woof-mcp/src/lib.rs", ["http://127.0.0.1:3334"]],
    ["apps/woof/src-tauri/src/commands.rs", ["http://127.0.0.1:3334"]],
    ["apps/woof/src-tauri/src/supervisor.rs", ["http://127.0.0.1:3334/health"]],
    [
      "crates/woof-llm/src/endpoint.rs",
      [
        "https://api.openai.com/v1/chat/completions",
        "wss://api.openai.com/v1/realtime?intent=transcription",
      ],
    ],
  ]);
  const expectedAllDestinations = new Map([
    ["apps/woof-mcp/src/lib.rs", ["http://127.0.0.1:3334"]],
    [
      "apps/woof/src-tauri/src/commands.rs",
      [
        "http://127.0.0.1:3334",
        "https://example.test/?token=must-not-be-in-context",
        "https://memory-hub/followups",
      ],
    ],
    ["apps/woof/src-tauri/src/notifications.rs", ["https://example.com"]],
    ["apps/woof/src-tauri/src/supervisor.rs", ["http://127.0.0.1:3334/health"]],
    ["apps/woof_d/src/memory.rs", ["https://example.test"]],
    [
      "crates/woof-capture/src/engine.rs",
      [
        "https://example.com/?owner=private@example.com",
        "https://example.com/?owner=[REDACTED_EMAIL]",
      ],
    ],
    [
      "crates/woof-capture/src/macos.rs",
      [
        "https://secret.example.com/payroll",
        "https://allowed.example/work",
        "https://allowed.example/work",
        "https://secret.example.com/payroll",
        "https://allowed.example/one",
        "https://allowed.example/one",
        "https://allowed.example/two",
      ],
    ],
    [
      "crates/woof-capture/src/policy.rs",
      [
        "https://secret.example.com/report",
        "https://secret.example.com./report",
        "https://[2001:db8::1]/report",
      ],
    ],
    [
      "crates/woof-core/src/config.rs",
      ["https://example.com", "https://example.com"],
    ],
    [
      "crates/woof-llm/src/endpoint.rs",
      [
        "https://api.openai.com/v1/chat/completions",
        "wss://api.openai.com/v1/realtime?intent=transcription",
        "http://api.openai.com/v1/chat/completions",
        "https://api.openai.com.evil.invalid/v1/chat/completions",
        "https://api.openai.com@evil.invalid/v1/chat/completions",
        "https://user:pass@api.openai.com/v1/chat/completions",
        "https://api.openai.com:8443/v1/chat/completions",
      ],
    ],
  ]);
  const primitivePattern =
    /\b(?:(?:reqwest::Client|Client)::(?:new|builder|default)\s*\(|(?:tokio::net::)?Tcp(?:Listener::bind|Stream::connect)\s*\(|(?:tokio::net::)?UdpSocket::(?:bind|connect)\s*\(|connect_async(?:_with_config)?\s*\(|(?:hyper|hyper_util)::client|ureq::(?:Agent|agent|get|post|request)|curl::easy|awc::Client|isahc::|surf::|libc::(?:socket|connect|bind|sendto)\s*\(|CFReadStreamCreateForHTTPRequest|NSURLSession|NWConnection|fetch\s*\(|axios(?:\.|\s*\()|new\s+(?:WebSocket|WebTransport|XMLHttpRequest|EventSource)\b|navigator\.sendBeacon\s*\(|Deno\.connect\s*\(|node:(?:http|https|net|dgram))/gu;
  const destinationPattern =
    /(?:https?|wss?|ftp|ftps|tcp|udp):\/\/(?:[A-Za-z0-9]|\[)[^"'`\s),;]*/gu;
  const observedPrimitives = new Map();
  const observedDestinations = new Map();
  const observedAllDestinations = new Map();

  for (const path of productionSources()) {
    const completeText = source(path);
    const text = productionText(path);
    const primitives = [...completeText.matchAll(primitivePattern)];
    if (primitives.length > 0) observedPrimitives.set(path, primitives.length);
    const destinations = [...text.matchAll(destinationPattern)].map((match) => match[0]);
    if (destinations.length > 0) observedDestinations.set(path, destinations);
    const allDestinations = [...completeText.matchAll(destinationPattern)].map(
      (match) => match[0],
    );
    if (allDestinations.length > 0) observedAllDestinations.set(path, allDestinations);
  }

  assert(
    JSON.stringify([...observedPrimitives]) === JSON.stringify([...expectedPrimitives]),
    "production network primitive inventory changed",
  );
  assert(
    JSON.stringify([...observedDestinations]) === JSON.stringify([...expectedDestinations]),
    "production network destination inventory changed",
  );
  assert(
    JSON.stringify([...observedAllDestinations]) ===
      JSON.stringify([...expectedAllDestinations]),
    "complete source URL inventory changed",
  );

  for (const path of expectedPrimitives.keys()) {
    const text = productionText(path);
    const networkRelevantEnvironment = text.replaceAll('env!("CARGO_PKG_VERSION")', "");
    assert(
      !/std::env::var(?:_os)?\s*\(|option_env!\s*\(|env!\s*\(/u.test(
        networkRelevantEnvironment,
      ),
      `${path} derives networking from the process environment`,
    );
  }

  requireSource("apps/woof-mcp/src/lib.rs", [
    'pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:3334"',
    "if daemon_url != DEFAULT_DAEMON_URL",
    ".no_proxy()",
    ".redirect(Policy::none())",
  ]);
  requireSource("apps/woof/src-tauri/src/commands.rs", [
    'const DAEMON_ORIGIN: &str = "http://127.0.0.1:3334"',
    "if !path.starts_with('/') || path.contains(\"://\")",
    ".no_proxy()",
    ".redirect(reqwest::redirect::Policy::none())",
  ]);
  requireSource("crates/woof-llm/src/endpoint.rs", [
    'pub const OPENAI_HOST: &str = "api.openai.com"',
    'pub const CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions"',
    '"wss://api.openai.com/v1/realtime?intent=transcription"',
  ]);

  const tauri = JSON.parse(source("apps/woof/src-tauri/tauri.conf.json"));
  const connectDirective = tauri.app.security.csp
    .split(";")
    .map((directive) => directive.trim())
    .find((directive) => directive.startsWith("connect-src "));
  assert(
    connectDirective === "connect-src 'self' ipc: http://ipc.localhost",
    "desktop content security policy permits an unexpected network destination",
  );
}

function descendantsOf(rows, rootPids) {
  const selected = new Set(rootPids);
  let changed = true;
  while (changed) {
    changed = false;
    for (const row of rows) {
      if (!selected.has(row.pid) && selected.has(row.ppid)) {
        selected.add(row.pid);
        changed = true;
      }
    }
  }
  return rows.filter((row) => selected.has(row.pid));
}

function staticAudit() {
  requireSource("crates/woof-core/src/config.rs", [
    'format!("127.0.0.1:{}", self.api_port)',
    "if self.api_port != 3334",
    "nudges_enabled: false",
    "data_retention: DataRetentionPolicy::KeepForever",
  ]);
  requireSource("apps/woof_d/src/main.rs", [
    "restrict_private_file_creation();",
    "umask(PRIVATE_FILE_UMASK);",
    "if address != SocketAddr::from(([127, 0, 0, 1], 3334))",
    "tokio::net::TcpListener::bind",
    "let listener = bind(address).await?;",
    "let startup = initialize()?;",
    "let listener_guard = retain_listener_lock(&listener)?;",
    "drop(listener_guard);",
  ]);
  const daemonMain = source("apps/woof_d/src/main.rs");
  assert(
    daemonMain.indexOf("let listener = bind(address).await?;") <
      daemonMain.indexOf("let startup = initialize()?;"),
    "daemon must acquire the exact listener before storage initialization",
  );
  const shutdownTail = daemonMain.slice(
    daemonMain.indexOf("let server_result = server_task.await;"),
  );
  const shutdownOrder = [
    "let server_result = server_task.await;",
    "supervisor.shutdown().await;",
    "automation_supervisor.shutdown().await;",
    "if let Some(supervisor) = memory_supervisor",
    "let shutdown_mutation_guard = state.storage_mutation_barrier().lock().await;",
    "drop(listener_guard);",
    "drop(shutdown_mutation_guard);",
    "let server_result = server_result?;",
  ].map((fragment) => shutdownTail.indexOf(fragment));
  assert(
    shutdownOrder.every(
      (offset, index) => offset >= 0 && (index === 0 || offset > shutdownOrder[index - 1]),
    ),
    "daemon listener guard does not outlive every accepted request and background mutator",
  );
  requireSource("apps/woof/src-tauri/src/main.rs", ["umask(0o077)"]);
  requireSource("apps/woof_d/src/lib.rs", [
    '.route("/health", get(health))',
    'request.method() == Method::GET && request.uri().path() == "/health"',
    "state.token.matches_bearer(candidate)",
    "health_proof(&state.token, challenge)",
  ]);
  requireSource("crates/woof-core/src/api_token.rs", [
    "use subtle::ConstantTimeEq",
    "let mut fixed = [0_u8; TOKEN_BYTES]",
    "candidate.len().min(TOKEN_BYTES)",
    "candidate.len() as u64).ct_eq(&(TOKEN_BYTES as u64))",
    "self.0.ct_eq(&fixed)",
    "fixed.zeroize()",
  ]);
  requireSource("crates/woof-core/src/health_proof.rs", [
    'pub const HEALTH_CHALLENGE_HEADER: &str = "x-woof-health-challenge"',
    'pub const HEALTH_PROOF_HEADER: &str = "x-woof-health-proof"',
    "hmac_sha256(token.expose(), &challenge_bytes)",
    "candidate.as_bytes().ct_eq(expected.as_bytes())",
  ]);
  requireSource("crates/woof-core/src/secure_file.rs", [
    "options.mode(0o600)",
    "Permissions::from_mode(0o600)",
  ]);
  requireSource("crates/woof-storage/src/lib.rs", [
    "repair_private_database_files",
    'const DATABASE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"]',
    "for suffix in DATABASE_SIDECAR_SUFFIXES",
    "Permissions::from_mode(0o600)",
  ]);
  requireSource("crates/woof-llm/src/endpoint.rs", [
    'pub const OPENAI_HOST: &str = "api.openai.com"',
    'pub const CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions"',
    '"wss://api.openai.com/v1/realtime?intent=transcription"',
  ]);
  requireSource("crates/woof-llm/src/chat.rs", [
    ".https_only(true)",
    ".no_proxy()",
    ".redirect(Policy::none())",
  ]);
  requireSource("apps/woof/src-tauri/src/supervisor.rs", [
    'const HEALTH_URL: &str = "http://127.0.0.1:3334/health"',
    "generate_health_challenge()",
    "verify_health_proof(&self.health_token, &challenge, proof)",
    'let mut arguments = vec!["--watch-parent-stdin"]',
    'arguments.push("--start-paused")',
    "inner.machine.begin_shutdown()",
    "let _ = child.kill()",
  ]);
  requireSource("apps/woof/src-tauri/src/lib.rs", [
    "handle.state::<DaemonSupervisor>().shutdown()",
    "app.set_activation_policy(tauri::ActivationPolicy::Accessory)",
  ]);
  requireSource("apps/woof/src-tauri/Info.plist", [
    "<key>LSUIElement</key>",
    "<key>LSMultipleInstancesProhibited</key>",
  ]);
  assert(
    /<key>LSUIElement<\/key>\s*<true\/>/.test(
      source("apps/woof/src-tauri/Info.plist"),
    ),
    "Info.plist does not declare woof as a menu-bar agent",
  );
  assert(
    /<key>LSMultipleInstancesProhibited<\/key>\s*<true\/>/.test(
      source("apps/woof/src-tauri/Info.plist"),
    ),
    "Info.plist does not prohibit multiple woof instances",
  );
  assertNetworkSourceInventory();
  assertExactRouteContracts();
}

async function liveAudit() {
  const home = userInfo().homedir;
  const configRoot = join(home, ".woof");
  const dataRoot = join(home, "Library", "Application Support", "woof");
  const tokenPath = join(configRoot, "api-token");
  const configPath = join(configRoot, "config.json");
  const databasePath = join(dataRoot, "woof.db");

  walkPrivate(configRoot);
  walkPrivate(dataRoot);

  const token = readFileSync(tokenPath, "ascii");
  assert(/^[0-9a-f]{64}$/.test(token), "local bearer token is malformed");
  const config = JSON.parse(readFileSync(configPath, "utf8"));
  assert(config.api_port === 3334, "configured daemon port is not 3334");
  assert(config.db_path === databasePath, "configured database path escaped woof state");
  assert(
    config.identity_path === join(dataRoot, "identity.json"),
    "configured identity path escaped woof state",
  );
  assert(config.log_dir === join(dataRoot, "logs"), "configured log path escaped woof state");

  const listener = execFileSync(
    "/usr/sbin/lsof",
    ["-nP", "-iTCP:3334", "-sTCP:LISTEN", "-Fn"],
    { encoding: "utf8" },
  );
  const listenerNames = listener
    .split("\n")
    .filter((line) => line.startsWith("n"))
    .map((line) => line.slice(1));
  assert(listenerNames.length === 1, "port 3334 must have exactly one listening socket");
  assert(listenerNames[0] === "127.0.0.1:3334", "daemon listener is not exact IPv4 loopback");

  const rows = processRows();
  const appCommand = "/Applications/woof.app/Contents/MacOS/woof";
  const daemonCommands = new Set([
    `${appCommand}_d --watch-parent-stdin`,
    `${appCommand}_d --watch-parent-stdin --start-paused`,
  ]);
  const apps = rows.filter((row) => row.command === appCommand);
  const daemons = rows.filter(
    (row) => row.command === `${appCommand}_d` || row.command.startsWith(`${appCommand}_d `),
  );
  assert(apps.length === 1, "expected exactly one installed woof application process");
  assert(daemons.length === 1, "expected exactly one supervised woof daemon process");
  assert(
    daemonCommands.has(daemons[0].command),
    "woof daemon command has unexpected arguments",
  );
  assert(daemons[0].ppid === apps[0].pid, "woof daemon is not parented by the application");

  const health = await httpRequest("GET", "/health");
  assert(health.status === 200, "public GET /health did not return HTTP 200");
  assert(health.body.equals(Buffer.from('{"status":"ok"}')), "public GET /health body changed");
  assert(
    health.headers["x-woof-health-proof"] === undefined,
    "unchallenged public health unexpectedly returned an ownership proof",
  );

  const challenge = randomBytes(32).toString("hex");
  const challengedHealth = await httpRequest("GET", "/health", {
    headers: { "x-woof-health-challenge": challenge },
  });
  assert(challengedHealth.status === 200, "challenged GET /health did not return HTTP 200");
  assert(
    challengedHealth.body.equals(Buffer.from('{"status":"ok"}')),
    "challenged GET /health body changed",
  );
  const proof = challengedHealth.headers["x-woof-health-proof"];
  assert(
    typeof proof === "string" && /^[0-9a-f]{64}$/.test(proof),
    "challenged GET /health did not return a canonical ownership proof",
  );
  const expectedProof = createHmac("sha256", Buffer.from(token, "ascii"))
    .update(Buffer.from(challenge, "hex"))
    .digest();
  assert(
    timingSafeEqual(Buffer.from(proof, "hex"), expectedProof),
    "challenged GET /health ownership proof was invalid",
  );

  const protectedCases = [
    await httpRequest("POST", "/health"),
    await httpRequest("GET", "/health/"),
    await httpRequest("GET", "/working-memory"),
    await httpRequest("GET", "/does-not-exist"),
    await httpRequest("GET", "/working-memory", { token: "short" }),
  ];
  assert(
    protectedCases.every((result) => result.status === 401),
    "authentication did not run before routing",
  );
  const authenticatedUnknown = await httpRequest("GET", "/does-not-exist", { token });
  assert(authenticatedUnknown.status === 404, "valid bearer token did not reach routing");

  const mcpCommand = "/Applications/woof.app/Contents/MacOS/woof-mcp";
  const permittedInstalledCommands = new Set([appCommand, ...daemonCommands, mcpCommand]);
  const installedPrefix = "/Applications/woof.app/Contents/MacOS/";
  assert(
    rows
      .filter((row) => row.command.startsWith(installedPrefix))
      .every((row) => permittedInstalledCommands.has(row.command)),
    "installed bundle launched an unexpected executable or argument set",
  );

  for (let sample = 0; sample < 4; sample += 1) {
    const sampledRows = processRows();
    const sampledApps = sampledRows.filter((row) => row.command === appCommand);
    const sampledDaemons = sampledRows.filter((row) => daemonCommands.has(row.command));
    const sampledMcp = sampledRows.filter((row) => row.command === mcpCommand);
    assert(sampledApps.length === 1, "application process changed during network sampling");
    assert(sampledDaemons.length === 1, "daemon process changed during network sampling");
    assert(
      sampledDaemons[0].ppid === sampledApps[0].pid,
      "daemon parent changed during network sampling",
    );
    assert(
      sampledRows
        .filter((row) => row.command.startsWith(installedPrefix))
        .every((row) => permittedInstalledCommands.has(row.command)),
      "network sampling found an unexpected bundled process",
    );
    const roots = [
      ...sampledApps.map((row) => row.pid),
      ...sampledDaemons.map((row) => row.pid),
      ...sampledMcp.map((row) => row.pid),
    ];
    for (const process of descendantsOf(sampledRows, roots)) {
      execFileSync(join(repo, "scripts", "verify-network-boundary.sh"), [String(process.pid)], {
        stdio: "ignore",
      });
    }
    if (sample < 3) await new Promise((resolveDelay) => setTimeout(resolveDelay, 200));
  }
}

try {
  staticAudit();
  if (live) await liveAudit();
  console.log(
    live
      ? "Runtime boundary audit passed (source and live)."
      : "Runtime boundary source audit passed.",
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
