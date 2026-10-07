import { performance } from "node:perf_hooks";

const base = process.env.API_BASE || "http://127.0.0.1:18765/api/v1";
const token = process.env.API_TOKEN;
const samples = parseInt(process.env.SAMPLES || "30", 10);

if (!token) {
  console.error("API_TOKEN environment variable is required.");
  process.exit(1);
}

async function measure(path) {
  const start = performance.now();
  let headersAt = start;
  const res = await fetch(`${base}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(8000),
  });
  headersAt = performance.now();
  await res.arrayBuffer();
  const totalMs = Math.round(performance.now() - start);
  const ttfbMs = Math.round(headersAt - start);
  return { path, status: res.status, ttfbMs, totalMs };
}

function calcStats(numbers) {
  const sorted = [...numbers].sort((a, b) => a - b);
  const min = sorted[0];
  const max = sorted[sorted.length - 1];
  const median = sorted[Math.floor(sorted.length / 2)];
  const p95 = sorted[Math.ceil(sorted.length * 0.95) - 1];
  return { min, median, p95, max };
}

async function main() {
  console.log(`Starting latency measurement against ${base} with ${samples} samples...`);

  const paths = ["/subscriptions", "/rule-sets", "/jobs?limit=50", "/auth/me"];
  const metrics = {};
  for (const p of paths) metrics[p] = [];

  for (let i = 0; i < samples; i++) {
    const batch = await Promise.all(paths.map(measure));
    for (const item of batch) {
      if (item.status !== 200) {
        console.error(`Received non-200 status for ${item.path}: ${item.status}`);
      }
      metrics[item.path].push(item.totalMs);
    }
  }

  console.log("\n--- Measurement Results (ms) ---");
  for (const p of paths) {
    const stats = calcStats(metrics[p]);
    console.log(`${p.padEnd(20)}: min=${stats.min}ms, median=${stats.median}ms, p95=${stats.p95}ms, max=${stats.max}ms`);
  }
}

main().catch((err) => {
  console.error("Measurement failed:", err);
  process.exit(1);
});
