import { appendFileSync } from "node:fs";

const outputPath = process.env.HERDR_OMP_TEST_REPORTS;
if (!outputPath) {
  process.exit(2);
}

const args = process.argv.slice(2);
const method = args.slice(0, 2).join(" ");
const delayMethod = process.env.HERDR_OMP_TEST_DELAY_METHOD;
const delayMs = Number.parseInt(process.env.HERDR_OMP_TEST_DELAY_MS || "0", 10);
if (delayMethod === method && Number.isFinite(delayMs) && delayMs > 0) {
  await Bun.sleep(delayMs);
}

appendFileSync(outputPath, `${JSON.stringify(args)}\n`, "utf8");
const failMethod = process.env.HERDR_OMP_TEST_FAIL_METHOD;
process.exit(failMethod === method ? 1 : 0);
