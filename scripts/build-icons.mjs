#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const master = resolve(process.argv[2] ?? join(root, "assets/generated/boxer-master-1024.png"));
const output = resolve(process.argv[3] ?? join(root, "assets/generated"));
const canonicalOutput = join(root, "assets/generated");
const iconset = join(output, "woof-app.iconset");
const menuBarSource = join(root, "assets/woof-menubar.svg");

const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function pngDimensions(path) {
  const bytes = readFileSync(path);
  if (!bytes.subarray(0, 8).equals(pngSignature)) {
    throw new Error(`${path} is not a PNG`);
  }
  return { bytes, width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

function resize(size, name) {
  const path = join(iconset, name);
  execFileSync("/usr/bin/sips", ["-z", String(size), String(size), master, "--out", path], {
    stdio: "ignore",
  });
  const image = pngDimensions(path);
  if (image.width !== size || image.height !== size) {
    throw new Error(`${name} has unexpected dimensions ${image.width}x${image.height}`);
  }
  return path;
}

function chunk(type, path, expectedSize) {
  const image = pngDimensions(path);
  if (image.width !== expectedSize || image.height !== expectedSize) {
    throw new Error(`${basename(path)} must be ${expectedSize}x${expectedSize}`);
  }
  const header = Buffer.alloc(8);
  header.write(type, 0, 4, "ascii");
  header.writeUInt32BE(image.bytes.length + 8, 4);
  return Buffer.concat([header, image.bytes]);
}

mkdirSync(output, { recursive: true });
mkdirSync(iconset, { recursive: true });

const sources = [
  ["icp4", resize(16, "icon_16x16.png"), 16],
  ["icp5", resize(32, "icon_32x32.png"), 32],
  ["icp6", resize(64, "icon_32x32@2x.png"), 64],
  ["ic07", resize(128, "icon_128x128.png"), 128],
  ["ic08", resize(256, "icon_256x256.png"), 256],
  ["ic09", resize(512, "icon_512x512.png"), 512],
  ["ic10", resize(1024, "icon_512x512@2x.png"), 1024],
];

resize(32, "icon_16x16@2x.png");
resize(256, "icon_128x128@2x.png");
resize(512, "icon_256x256@2x.png");

const chunks = sources.map(([type, path, size]) => chunk(type, path, size));
const totalLength = chunks.reduce((sum, value) => sum + value.length, 8);
const header = Buffer.alloc(8);
header.write("icns", 0, 4, "ascii");
header.writeUInt32BE(totalLength, 4);
writeFileSync(join(output, "woof.icns"), Buffer.concat([header, ...chunks]));

for (const [size, name] of [
  [18, "woof-menubar-Template.png"],
  [36, "woof-menubar-Template@2x.png"],
]) {
  execFileSync(
    "/opt/homebrew/bin/magick",
    [
      "-background",
      "none",
      menuBarSource,
      "-resize",
      `${size}x${size}`,
      "-alpha",
      "on",
      "-strip",
      "-define",
      "png:exclude-chunks=date,time",
      join(output, name),
    ],
    { stdio: "inherit" },
  );

  const image = pngDimensions(join(output, name));
  if (image.width !== size || image.height !== size) {
    throw new Error(`${name} has unexpected dimensions ${image.width}x${image.height}`);
  }
}

if (output === canonicalOutput) {
  const tauriIcons = join(root, "apps/woof/src-tauri/icons");
  const frontendMascot = join(root, "apps/woof/static/mascot");
  mkdirSync(tauriIcons, { recursive: true });
  mkdirSync(frontendMascot, { recursive: true });

  for (const [source, destination] of [
    [master, join(tauriIcons, "icon.png")],
    [join(output, "woof.icns"), join(tauriIcons, "icon.icns")],
    [join(output, "woof-menubar-Template.png"), join(tauriIcons, "woof-menubar-Template.png")],
    [join(output, "woof-menubar-Template@2x.png"), join(tauriIcons, "woof-menubar-Template@2x.png")],
    [master, join(frontendMascot, "boxer-head.png")],
  ]) {
    copyFileSync(source, destination);
    if (!readFileSync(source).equals(readFileSync(destination))) {
      throw new Error(`staged icon does not match ${basename(source)}`);
    }
  }
}

console.log(`Built woof icons from ${master}`);
