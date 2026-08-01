#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync,
  constants,
  existsSync,
  fstatSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readlinkSync,
  readSync,
  readdirSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir, userInfo } from "node:os";
import { basename, isAbsolute, join, relative, resolve, sep } from "node:path";

const encodedSignatures = [
  "ZmlzaA==",
  "YXF1YXJpdW0=",
  "dGFuay0=",
  "YnViYmxl",
  "a2FzcGk=",
  "bW9kZWwydmVj",
  "cG90aW9uLWJhc2U=",
  "cmVjb3ZlcmVk",
  "cHJlZGVjZXNzb3I=",
  "cHJlLWltcG9ydA==",
  "c291cmNlX3ZlcnNpb24=",
  "bWVtb3J5X2ltcG9ydA==",
  "c2V0dXAtcGFyaXR5",
  "RGVuLnN2ZWx0ZQ==",
  "dmlldz1kZW4=",
  "cGFyaXR5",
  "Z29sZGZpc2guYXBw",
  "Y29tLmthc3BpLmdvbGRmaXNo",
];

const encodedExcludedRoots = [
  "L0FwcGxpY2F0aW9ucy9Hb2xkZmlzaC5hcHA=",
  "TGlicmFyeS9BcHBsaWNhdGlvbiBTdXBwb3J0L0dvbGRmaXNo",
  "LmdvbGRmaXNo",
  "TGlicmFyeS9DYWNoZXMvY29tLmthc3BpLmdvbGRmaXNo",
  "TGlicmFyeS9Mb2dzL0dvbGRmaXNo",
  "TGlicmFyeS9QcmVmZXJlbmNlcy9jb20ua2FzcGkuZ29sZGZpc2gucGxpc3Q=",
  "TGlicmFyeS9TYXZlZCBBcHBsaWNhdGlvbiBTdGF0ZS9jb20ua2FzcGkuZ29sZGZpc2guc2F2ZWRTdGF0ZQ==",
  "TGlicmFyeS9XZWJLaXQvY29tLmthc3BpLmdvbGRmaXNo",
  "TGlicmFyeS9IVFRQU3RvcmFnZXMvY29tLmthc3BpLmdvbGRmaXNo",
  "TGlicmFyeS9Db250YWluZXJzL2NvbS5rYXNwaS5nb2xkZmlzaA==",
];

const asciiSignatures = encodedSignatures.map((encoded) =>
  Buffer.from(Buffer.from(encoded, "base64").toString("ascii").toLowerCase(), "ascii"),
);
const signatureVariants = asciiSignatures.flatMap((signature, signatureIndex) => {
  const text = signature.toString("ascii");
  const utf16LittleEndian = Buffer.from(text, "utf16le");
  const utf16BigEndian = Buffer.allocUnsafe(utf16LittleEndian.length);
  for (let index = 0; index < utf16LittleEndian.length; index += 2) {
    utf16BigEndian[index] = utf16LittleEndian[index + 1];
    utf16BigEndian[index + 1] = utf16LittleEndian[index];
  }
  return [
    { bytes: signature, signatureIndex },
    { bytes: utf16LittleEndian, signatureIndex },
    { bytes: utf16BigEndian, signatureIndex },
  ];
});
const maximumSignatureLength = Math.max(
  ...signatureVariants.map((signature) => signature.bytes.length),
);
const chunkSize = 64 * 1024;

function gitEnvironment() {
  const environment = { ...process.env };
  for (const name of Object.keys(environment)) {
    if (name.startsWith("GIT_")) delete environment[name];
  }
  environment.GIT_NO_REPLACE_OBJECTS = "1";
  environment.GIT_NO_LAZY_FETCH = "1";
  environment.GIT_OPTIONAL_LOCKS = "0";
  environment.GIT_CONFIG_NOSYSTEM = "1";
  environment.GIT_CONFIG_GLOBAL = "/dev/null";
  return environment;
}

function excludedRoots() {
  const accountHome = userInfo().homedir;
  return encodedExcludedRoots.map((encoded) => {
    const decoded = Buffer.from(encoded, "base64").toString("utf8");
    return resolve(decoded.startsWith(sep) ? decoded : join(accountHome, decoded));
  });
}

function lowerAscii(buffer) {
  const lowered = Buffer.from(buffer);
  for (let index = 0; index < lowered.length; index += 1) {
    if (lowered[index] >= 0x41 && lowered[index] <= 0x5a) lowered[index] += 0x20;
  }
  return lowered;
}

function digestLabel(label) {
  return createHash("sha256").update(label).digest("hex").slice(0, 16);
}

function runGit(repo, argumentsList, options = {}) {
  const result = spawnSync("/usr/bin/git", ["--no-replace-objects", "-C", repo, ...argumentsList], {
    encoding: options.encoding ?? null,
    env: gitEnvironment(),
    maxBuffer: 1024 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Git command failed while auditing repository metadata (${argumentsList[0]}).`);
  }
  return result.stdout;
}

function pathContains(parent, child) {
  const parentKey = resolve(parent).toLowerCase();
  const childKey = resolve(child).toLowerCase();
  return childKey === parentKey || childKey.startsWith(`${parentKey}${sep}`);
}

function assertAllowedRoot(root) {
  const absolute = resolve(root);
  for (const excluded of excludedRoots()) {
    if (pathContains(absolute, excluded) || pathContains(excluded, absolute)) {
      throw new Error("Refusing to inspect an excluded installed-application or runtime-data path.");
    }
  }
  return absolute;
}

class Matcher {
  constructor(audit, label) {
    this.audit = audit;
    this.label = label;
    this.tail = Buffer.alloc(0);
    this.detected = new Set();
  }

  push(chunk) {
    if (chunk.length === 0) return;
    const combined = Buffer.concat([this.tail, chunk]);
    const lowered = lowerAscii(combined);
    for (const signature of signatureVariants) {
      if (
        !this.detected.has(signature.signatureIndex) &&
        lowered.indexOf(signature.bytes) !== -1
      ) {
        this.detected.add(signature.signatureIndex);
        this.audit.record(this.label, signature.signatureIndex);
      }
    }
    const retained = Math.min(maximumSignatureLength - 1, combined.length);
    this.tail = Buffer.from(combined.subarray(combined.length - retained));
  }
}

class Audit {
  constructor() {
    this.findings = [];
  }

  record(label, signatureIndex) {
    this.findings.push({
      entry: label.startsWith("git:object:")
        ? `object ${label.slice("git:object:".length)}`
        : `entry ${digestLabel(label)}`,
      signatureIndex: signatureIndex + 1,
    });
  }

  scanBytes(label, bytes) {
    const matcher = new Matcher(this, label);
    matcher.push(bytes);
  }

  scanFile(label, path) {
    const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const buffer = Buffer.allocUnsafe(chunkSize);
    const matcher = new Matcher(this, label);
    try {
      if (!fstatSync(descriptor).isFile()) {
        throw new Error("Audited file descriptor is not a regular file.");
      }
      while (true) {
        const bytesRead = readSync(descriptor, buffer, 0, buffer.length, null);
        if (bytesRead === 0) break;
        matcher.push(buffer.subarray(0, bytesRead));
      }
    } finally {
      closeSync(descriptor);
    }
  }

  scanEntry(root, relativePath, { missingIsAllowed = false } = {}) {
    this.scanBytes(`path:${relativePath}`, Buffer.from(relativePath, "utf8"));
    const path = join(root, relativePath);
    let metadata;
    try {
      metadata = lstatSync(path);
    } catch (error) {
      if (missingIsAllowed && error?.code === "ENOENT") return;
      throw new Error("Could not inspect an audited filesystem entry.");
    }
    if (metadata.isSymbolicLink()) {
      this.scanBytes(`link:${relativePath}`, Buffer.from(readlinkSync(path), "utf8"));
    } else if (metadata.isFile()) {
      this.scanFile(`file:${relativePath}`, path);
    } else {
      throw new Error("Git source entries must be regular files or symbolic links.");
    }
  }

  assertClean(context) {
    if (this.findings.length === 0) return;
    const preview = this.findings
      .slice(0, 20)
      .map(
        (finding) =>
          `pattern #${finding.signatureIndex} in ${finding.entry}`,
      )
      .join("\n");
    const suffix = this.findings.length > 20 ? `\n${this.findings.length - 20} additional matches` : "";
    throw new Error(`${context} failed with ${this.findings.length} encoded-signature match(es):\n${preview}${suffix}`);
  }
}

function validateRelativePath(path) {
  if (
    path.length === 0 ||
    path.includes("\0") ||
    isAbsolute(path) ||
    path.split(/[\\/]/u).includes("..")
  ) {
    throw new Error("Git returned an unsafe source path.");
  }
}

function auditSource(repo) {
  const requested = assertAllowedRoot(repo);
  const root = assertAllowedRoot(
    runGit(requested, ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim(),
  );
  const listed = runGit(root, ["ls-files", "--cached", "--others", "--exclude-standard", "-z"]);
  const audit = new Audit();
  for (const pathBytes of listed.subarray(0, Math.max(0, listed.length - 1)).toString("utf8").split("\0")) {
    if (pathBytes.length === 0) continue;
    validateRelativePath(pathBytes);
    audit.scanEntry(root, pathBytes, { missingIsAllowed: true });
  }
  audit.assertClean("Source path-and-byte audit");
  console.log("Source path-and-byte audit passed.");
}

function walkTree(audit, root, current = root) {
  const relativePath = relative(root, current) || basename(root);
  audit.scanBytes(`path:${relativePath}`, Buffer.from(relativePath, "utf8"));
  const metadata = lstatSync(current);
  if (metadata.isSymbolicLink()) {
    audit.scanBytes(`link:${relativePath}`, Buffer.from(readlinkSync(current), "utf8"));
    return;
  }
  if (metadata.isFile()) {
    audit.scanFile(`file:${relativePath}`, current);
    return;
  }
  if (!metadata.isDirectory()) {
    throw new Error("Audited trees may contain only directories, regular files, and symbolic links.");
  }
  const entries = readdirSync(current, { withFileTypes: true }).sort((left, right) =>
    Buffer.from(left.name).compare(Buffer.from(right.name)),
  );
  for (const entry of entries) walkTree(audit, root, join(current, entry.name));
}

function auditTrees(roots) {
  const audit = new Audit();
  for (const requested of roots) {
    const root = assertAllowedRoot(requested);
    if (!existsSync(root)) throw new Error("An audited tree root does not exist.");
    walkTree(audit, root);
  }
  audit.assertClean("Artifact or runtime tree audit");
  console.log(`Artifact or runtime tree audit passed for ${roots.length} root(s).`);
}

function scanGitObjects(audit, repo, objects) {
  for (const object of objects) {
    const bytes = runGit(repo, ["cat-file", object.type, object.id]);
    if (bytes.length !== object.size) {
      throw new Error("Git object content length differs from the all-object ledger.");
    }
    audit.scanBytes(`git:object:${object.id}`, bytes);
  }
}

function gitPath(repo, name) {
  return resolve(
    repo,
    runGit(repo, ["rev-parse", "--path-format=absolute", "--git-path", name], {
      encoding: "utf8",
    }).trim(),
  );
}

function assertNoAlternates(repo) {
  const objects = gitPath(repo, "objects");
  const objectMetadata = lstatSync(objects);
  if (objectMetadata.isSymbolicLink() || !objectMetadata.isDirectory()) {
    throw new Error("Git object storage must be a local directory, not a symbolic link.");
  }
  for (const name of ["alternates", "http-alternates"]) {
    const alternates = gitPath(repo, `objects/info/${name}`);
    if (existsSync(alternates)) {
      const metadata = lstatSync(alternates);
      if (metadata.isSymbolicLink() || !metadata.isFile() || metadata.size !== 0) {
        throw new Error("Git object alternates are not allowed for the final audit.");
      }
    }
  }
  return objects;
}

function isRawGitObjectStorageFile(objects, candidate) {
  const relativePath = relative(objects, resolve(candidate));
  if (relativePath.length === 0 || isAbsolute(relativePath) || relativePath.startsWith(`..${sep}`)) {
    return false;
  }
  const components = relativePath.split(sep);
  if (
    components.length === 2 &&
    /^[0-9a-f]{2}$/u.test(components[0]) &&
    /^(?:[0-9a-f]{38}|[0-9a-f]{62})$/u.test(components[1])
  ) {
    return true;
  }
  if (components.length === 2 && components[0] === "pack") {
    return (
      /^pack-(?:[0-9a-f]{40}|[0-9a-f]{64})\.(?:bitmap|idx|mtimes|pack|rev)$/u.test(
        components[1],
      ) ||
      /^multi-pack-index(?:-(?:[0-9a-f]{40}|[0-9a-f]{64})\.(?:bitmap|rev))?$/u.test(
        components[1],
      )
    );
  }
  if (
    components.length === 3 &&
    components[0] === "pack" &&
    components[1] === "multi-pack-index.d"
  ) {
    return /^multi-pack-index-(?:[0-9a-f]{40}|[0-9a-f]{64})\.midx$/u.test(components[2]);
  }
  return (
    (components.length === 2 &&
      components[0] === "info" &&
      components[1] === "commit-graph") ||
    (components.length === 3 &&
      components[0] === "info" &&
      components[1] === "commit-graphs" &&
      /^graph-(?:[0-9a-f]{40}|[0-9a-f]{64})\.graph$/u.test(components[2]))
  );
}

function assertNoObjectGarbage(repo) {
  const summary = runGit(repo, ["count-objects", "-v"], { encoding: "utf8" });
  const fields = new Map(
    summary
      .trim()
      .split("\n")
      .map((line) => line.match(/^([a-z-]+): ([0-9]+)$/u))
      .filter((match) => match !== null)
      .map((match) => [match[1], Number(match[2])]),
  );
  if (fields.get("garbage") !== 0 || fields.get("size-garbage") !== 0) {
    throw new Error("Git object storage contains non-object garbage.");
  }
}

function assertCanonicalObjectStorage(objects) {
  const pending = [objects];
  while (pending.length > 0) {
    const current = pending.pop();
    const metadata = lstatSync(current);
    const relativePath = relative(objects, current);
    if (metadata.isSymbolicLink()) {
      throw new Error("Git object storage must not contain symbolic links.");
    }
    if (metadata.isDirectory()) {
      const canonicalDirectory =
        relativePath.length === 0 ||
        relativePath === "info" ||
        relativePath === "pack" ||
        relativePath === join("pack", "multi-pack-index.d") ||
        relativePath === join("info", "commit-graphs") ||
        /^[0-9a-f]{2}$/u.test(relativePath);
      if (!canonicalDirectory) {
        throw new Error("Git object storage contains a noncanonical directory.");
      }
      for (const entry of readdirSync(current)) pending.push(join(current, entry));
      continue;
    }
    if (!metadata.isFile()) {
      throw new Error("Git object storage contains an unsupported entry type.");
    }
    const components = relativePath.split(sep);
    const canonicalTextMetadata =
      [
        join("info", "alternates"),
        join("info", "http-alternates"),
        join("info", "packs"),
        join("info", "commit-graphs", "commit-graph-chain"),
        join("pack", "multi-pack-index.d", "multi-pack-index-chain"),
      ].includes(relativePath) ||
      (components.length === 2 &&
        components[0] === "pack" &&
        /^pack-(?:[0-9a-f]{40}|[0-9a-f]{64})\.(?:keep|promisor)$/u.test(components[1]));
    if (!isRawGitObjectStorageFile(objects, current) && !canonicalTextMetadata) {
      throw new Error("Git object storage contains a noncanonical file.");
    }
  }
}

function assertCompleteRepository(repo) {
  const shallow = gitPath(repo, "shallow");
  if (existsSync(shallow) && statSync(shallow).size !== 0) {
    throw new Error("A shallow Git repository cannot satisfy the final audit.");
  }
  const reachableObjects = runGit(repo, ["rev-list", "--objects", "--all", "--missing=print"], {
    encoding: "utf8",
  });
  if (reachableObjects.split("\n").some((line) => line.startsWith("?"))) {
    throw new Error("Git is missing at least one reachable object.");
  }
}

function assertPruned(repo) {
  const result = spawnSync(
    "/usr/bin/git",
    ["--no-replace-objects", "-C", repo, "fsck", "--full", "--unreachable", "--no-reflogs"],
    { encoding: "utf8", env: gitEnvironment(), maxBuffer: 1024 * 1024 * 1024 },
  );
  if (result.error || result.status !== 0) throw new Error("Git object pruning check failed.");
  if (/\b(?:unreachable|dangling)\b/u.test(`${result.stdout}\n${result.stderr}`)) {
    throw new Error("Git still contains unreachable objects.");
  }
  const logs = gitPath(repo, "logs");
  if (existsSync(logs)) {
    const pending = [logs];
    while (pending.length > 0) {
      const path = pending.pop();
      const metadata = lstatSync(path);
      if (metadata.isSymbolicLink()) throw new Error("Git logs must not contain symbolic links.");
      if (metadata.isDirectory()) {
        for (const entry of readdirSync(path)) pending.push(join(path, entry));
      } else if (metadata.isFile() && metadata.size !== 0) {
        throw new Error("Git reflogs must be expired before the final audit.");
      }
    }
  }
}

async function auditGit(repo, { requirePruned = false } = {}) {
  const root = assertAllowedRoot(repo);
  runGit(root, ["rev-parse", "--git-dir"]);
  const gitDirectory = assertAllowedRoot(gitPath(root, "."));
  const commonDirectory = assertAllowedRoot(
    resolve(
      root,
      runGit(root, ["rev-parse", "--path-format=absolute", "--git-common-dir"], {
        encoding: "utf8",
      }).trim(),
    ),
  );
  const primaryObjectDatabase = resolve(assertNoAlternates(root));
  assertCompleteRepository(root);
  if (requirePruned) assertPruned(root);

  const audit = new Audit();
  const references = runGit(
    root,
    ["for-each-ref", "--format=%(refname)%00%(objectname)%00%(subject)%00"],
  );
  audit.scanBytes("git:reference-ledger", references);

  const metadataRoots = new Set([gitDirectory, commonDirectory]);
  for (const metadataRoot of metadataRoots) {
    if (!existsSync(metadataRoot)) throw new Error("Git metadata root is missing.");
    walkTree(audit, metadataRoot);
  }

  const objectLedger = runGit(root, [
    "cat-file",
    "--batch-all-objects",
    "--batch-check=%(objectname) %(objecttype) %(objectsize)",
  ]).toString("utf8");
  const objects = objectLedger
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^([0-9a-f]{40,64}) (blob|tree|commit|tag) (\d+)$/u);
      if (match === null) throw new Error("Git returned an invalid all-object ledger.");
      return { id: match[1], type: match[2], size: Number(match[3]) };
    });
  scanGitObjects(audit, root, objects);
  audit.assertClean("Git metadata-and-object audit");
  assertCanonicalObjectStorage(primaryObjectDatabase);
  assertNoObjectGarbage(commonDirectory);
  console.log(`Git metadata-and-object audit passed for ${objects.length} object(s).`);
}

function git(repo, argumentsList, input) {
  const result = spawnSync("/usr/bin/git", ["--no-replace-objects", "-C", repo, ...argumentsList], {
    input,
    encoding: input === undefined ? "utf8" : null,
    env: gitEnvironment(),
  });
  if (result.error || result.status !== 0) throw new Error("Self-test Git setup failed.");
  return result.stdout;
}

async function selfTest() {
  const temporary = mkdtempSync(join(tmpdir(), "woof-zero-audit-"));
  try {
    const tree = join(temporary, "tree");
    const cleanPath = join(tree, "clean.txt");
    const mkdir = spawnSync("/bin/mkdir", ["-p", tree]);
    if (mkdir.error || mkdir.status !== 0) throw new Error("Self-test directory setup failed.");
    writeFileSync(cleanPath, "clean release input\n", { mode: 0o600 });
    const clean = new Audit();
    walkTree(clean, tree);
    clean.assertClean("Clean self-test");

    const signature = Buffer.from(asciiSignatures[0]);
    const boundaryPath = join(tree, "boundary.bin");
    writeFileSync(
      boundaryPath,
      Buffer.concat([Buffer.alloc(chunkSize - 2, 0x78), signature, Buffer.from("\n")]),
      { mode: 0o600 },
    );
    const boundary = new Audit();
    walkTree(boundary, tree);
    if (boundary.findings.length === 0) throw new Error("Chunk-boundary self-test did not detect a match.");
    rmSync(boundaryPath);

    const widePath = join(tree, "wide.bin");
    writeFileSync(widePath, Buffer.from(signature.toString("ascii").toUpperCase(), "utf16le"), {
      mode: 0o600,
    });
    const wide = new Audit();
    walkTree(wide, tree);
    if (wide.findings.length === 0) throw new Error("Wide-byte self-test did not detect a match.");
    rmSync(widePath);

    const pathMatch = join(tree, `${signature.toString("ascii")}.txt`);
    writeFileSync(pathMatch, "clean bytes\n", { mode: 0o600 });
    const paths = new Audit();
    walkTree(paths, tree);
    if (paths.findings.length === 0) throw new Error("Path self-test did not detect a match.");
    rmSync(pathMatch);

    const external = join(temporary, "external.bin");
    const safeLink = join(tree, "safe-link");
    writeFileSync(external, signature, { mode: 0o600 });
    symlinkSync("../external.bin", safeLink);
    const noFollow = new Audit();
    walkTree(noFollow, tree);
    noFollow.assertClean("Symlink no-follow self-test");
    rmSync(safeLink);
    symlinkSync(signature.toString("ascii"), safeLink);
    const linkText = new Audit();
    walkTree(linkText, tree);
    if (linkText.findings.length === 0) {
      throw new Error("Symlink-target self-test did not detect a match.");
    }
    rmSync(safeLink);

    const repository = join(temporary, "repository");
    git(temporary, ["init", "-q", "-b", "main", repository]);
    git(repository, ["config", "user.name", "woof audit"]);
    git(repository, ["config", "user.email", "audit@invalid.example"]);
    writeFileSync(join(repository, "clean.txt"), "clean commit\n", { mode: 0o600 });
    git(repository, ["add", "clean.txt"]);
    git(repository, ["commit", "-q", "-m", "clean commit"]);

    const auxiliaryMetadata = join(gitPath(repository, "."), "audit-fixture");
    const auxiliaryObjects = join(auxiliaryMetadata, "objects");
    mkdirSync(auxiliaryObjects, { recursive: true, mode: 0o700 });
    writeFileSync(join(auxiliaryObjects, "remnant.bin"), signature, { mode: 0o600 });
    let caught = false;
    try {
      await auditGit(repository);
    } catch (error) {
      const fixtureEntry = digestLabel("file:audit-fixture/objects/remnant.bin");
      caught = String(error).includes(`pattern #1 in entry ${fixtureEntry}`);
    }
    if (!caught) {
      throw new Error("Auxiliary objects-directory self-test did not detect a match.");
    }
    rmSync(auxiliaryMetadata, { recursive: true, force: true });

    const primaryObjectsFixture = join(gitPath(repository, "objects"), "audit-fixture");
    const nestedPrimaryObjects = join(primaryObjectsFixture, "objects");
    mkdirSync(nestedPrimaryObjects, { recursive: true, mode: 0o700 });
    writeFileSync(join(nestedPrimaryObjects, "remnant.bin"), signature, { mode: 0o600 });
    caught = false;
    try {
      await auditGit(repository);
    } catch (error) {
      const fixtureEntry = digestLabel(
        "file:objects/audit-fixture/objects/remnant.bin",
      );
      caught = String(error).includes(`pattern #1 in entry ${fixtureEntry}`);
    }
    if (!caught) {
      throw new Error("Primary objects-directory self-test did not detect a match.");
    }
    writeFileSync(join(nestedPrimaryObjects, "remnant.bin"), "clean bytes\n", { mode: 0o600 });
    caught = false;
    try {
      await auditGit(repository);
    } catch (error) {
      caught = /noncanonical directory/u.test(String(error));
    }
    if (!caught) {
      throw new Error("Primary object-storage shape self-test accepted an unknown directory.");
    }
    rmSync(primaryObjectsFixture, { recursive: true, force: true });

    const objectId = git(repository, ["hash-object", "-w", "--stdin"], signature)
      .toString("utf8")
      .trim();
    caught = false;
    try {
      await auditGit(repository);
    } catch (error) {
      caught = String(error).includes(`pattern #1 in object ${objectId}`);
    }
    if (!caught) throw new Error("Unreachable-object self-test did not detect a match.");
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
  console.log("Zero-remnant audit self-test passed.");
}

function usage() {
  console.error(
    "usage: scripts/audit-zero-remnants.mjs source [REPOSITORY] | tree ROOT... | git [REPOSITORY] [--require-pruned] | self-test",
  );
}

try {
  const [mode, ...argumentsList] = process.argv.slice(2);
  if (mode === "source") {
    if (argumentsList.length > 1) throw new Error("source accepts at most one repository path");
    auditSource(argumentsList[0] ?? process.cwd());
  } else if (mode === "tree") {
    if (argumentsList.length === 0) throw new Error("tree requires at least one root");
    auditTrees(argumentsList);
  } else if (mode === "git") {
    const requirePruned = argumentsList.includes("--require-pruned");
    const paths = argumentsList.filter((argument) => argument !== "--require-pruned");
    if (paths.length > 1) throw new Error("git accepts at most one repository path");
    await auditGit(paths[0] ?? process.cwd(), { requirePruned });
  } else if (mode === "self-test" && argumentsList.length === 0) {
    await selfTest();
  } else {
    usage();
    process.exit(64);
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
