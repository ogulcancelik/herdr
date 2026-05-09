import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const sdkRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const repoRoot = dirname(dirname(sdkRoot));

test("typed method map covers the Rust socket Method enum", async () => {
  const [rustSchema, sdkSource] = await Promise.all([readRustSchema(), readSdkSource()]);

  assert.deepEqual(
    extractRustSerdeRenames(rustEnumBody(rustSchema, "Method")),
    extractTsMapKeys(tsTypeBody(sdkSource, "HerdrMethodParams")),
  );
});

test("typed subscriptions cover the Rust Subscription enum", async () => {
  const [rustSchema, sdkSource] = await Promise.all([readRustSchema(), readSdkSource()]);

  assert.deepEqual(
    extractRustSerdeRenames(rustEnumBody(rustSchema, "Subscription")),
    extractTsSubscriptionTypes(sdkSource),
  );
});

test("typed response discriminants cover the Rust ResponseResult enum", async () => {
  const [rustSchema, sdkSource] = await Promise.all([readRustSchema(), readSdkSource()]);

  assert.deepEqual(
    extractRustVariantNames(rustEnumBody(rustSchema, "ResponseResult")).map(toSnakeCase).sort(),
    extractTsResultTypes(sdkSource),
  );
});

async function readRustSchema() {
  return readFile(join(repoRoot, "src/api/schema.rs"), "utf8");
}

async function readSdkSource() {
  return readFile(join(sdkRoot, "src/index.ts"), "utf8");
}

function rustEnumBody(source, enumName) {
  const start = source.indexOf(`pub enum ${enumName} {`);
  assert.notEqual(start, -1, `missing Rust enum ${enumName}`);

  const bodyStart = source.indexOf("{", start) + 1;
  let depth = 1;
  for (let index = bodyStart; index < source.length; index += 1) {
    if (source[index] === "{") {
      depth += 1;
    } else if (source[index] === "}") {
      depth -= 1;
      if (depth === 0) {
        return source.slice(bodyStart, index);
      }
    }
  }

  throw new Error(`unterminated Rust enum ${enumName}`);
}

function tsTypeBody(source, typeName) {
  const start = source.indexOf(`export type ${typeName} = {`);
  assert.notEqual(start, -1, `missing TypeScript type ${typeName}`);

  const bodyStart = source.indexOf("{", start) + 1;
  const bodyEnd = source.indexOf("\n};", bodyStart);
  assert.notEqual(bodyEnd, -1, `unterminated TypeScript type ${typeName}`);
  return source.slice(bodyStart, bodyEnd);
}

function extractRustSerdeRenames(body) {
  return [...body.matchAll(/#\[serde\(rename = "([^"]+)"\)\]/g)]
    .map((match) => match[1])
    .sort();
}

function extractRustVariantNames(body) {
  return [...body.matchAll(/^\s{4}([A-Z][A-Za-z0-9]*)\s*(?:\{|,)/gm)].map((match) => match[1]);
}

function extractTsMapKeys(body) {
  return [...body.matchAll(/^\s{2}(?:"([^"]+)"|([a-z][a-z_]*)):/gm)]
    .map((match) => match[1] ?? match[2])
    .sort();
}

function extractTsSubscriptionTypes(source) {
  const body = source.slice(
    source.indexOf("export type Subscription ="),
    source.indexOf("export interface EventsSubscribeParams"),
  );
  assert.ok(body.startsWith("export type Subscription ="), "missing TypeScript Subscription type");

  return [...body.matchAll(/type: "([^"]+)"/g)].map((match) => match[1]).sort();
}

function extractTsResultTypes(source) {
  const body = source.slice(
    source.indexOf("export interface PongResult"),
    source.indexOf("export type HerdrMethodParams"),
  );
  assert.ok(body.startsWith("export interface PongResult"), "missing TypeScript result interfaces");

  return [...body.matchAll(/type: "([^"]+)"/g)].map((match) => match[1]).sort();
}

function toSnakeCase(value) {
  return value.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}
