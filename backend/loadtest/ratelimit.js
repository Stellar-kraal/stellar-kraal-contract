/**
 * Load test for the rate limiter (acceptance criterion: 100 req/s sustained
 * triggers 429 for exceeding clients while compliant clients are unaffected).
 *
 *   npm run loadtest
 *
 * Boots the API on an ephemeral port with production limits, then runs two
 * autocannon clients concurrently against GET /marketplace/listings:
 *
 *   - flooder:   ~100 req/s for 10 s from IP 10.99.0.66
 *   - compliant: 1 req/s for 10 s from IP 10.99.0.7
 *
 * Exits non-zero unless the flooder is throttled to (roughly) the configured
 * budget and the compliant client sees zero 429s.
 */
/* eslint-disable no-console */
require('ts-node/register');
const autocannon = require('autocannon');
const { createApp } = require('../src/app');
const { RATE_LIMIT_CONFIG } = require('../src/config/rateLimits');

async function main() {
  const { app } = createApp({ trustProxy: true });
  const server = app.listen(0);
  await new Promise((resolve) => server.once('listening', resolve));
  const { port } = server.address();
  const url = `http://127.0.0.1:${port}/marketplace/listings`;

  const DURATION = 10;
  const run = (rate, ip) =>
    autocannon({
      url,
      duration: DURATION,
      connections: Math.min(10, rate),
      overallRate: rate,
      headers: { 'x-forwarded-for': ip },
    });

  console.log(`target: ${url}`);
  console.log('flooder: 100 req/s | compliant: 1 req/s | duration: 10s\n');

  const [flood, compliant] = await Promise.all([run(100, '10.99.0.66'), run(1, '10.99.0.7')]);
  server.close();

  const ok2xx = (r) => r['2xx'];
  const rejected = (r) => r.non2xx;

  console.log(`flooder:   ${flood.requests.total} sent, ${ok2xx(flood)} allowed, ${rejected(flood)} throttled (429)`);
  console.log(`compliant: ${compliant.requests.total} sent, ${ok2xx(compliant)} allowed, ${rejected(compliant)} throttled (429)`);

  // The ip-burst budget bounds what a flooder can get through: with
  // limit L per W seconds, ~L*(DURATION/W) successes (+ margin for
  // sliding-window boundary effects).
  const { limit, windowSeconds } = RATE_LIMIT_CONFIG.tiers.ipBurst;
  const floodBudget = Math.ceil(limit * (DURATION / windowSeconds + 1));

  const failures = [];
  if (rejected(compliant) !== 0) {
    failures.push(`compliant client was throttled ${rejected(compliant)} times (expected 0)`);
  }
  if (rejected(flood) === 0) {
    failures.push('flooder was never throttled');
  }
  if (ok2xx(flood) > floodBudget) {
    failures.push(`flooder got ${ok2xx(flood)} requests through (budget ~${floodBudget})`);
  }

  if (failures.length > 0) {
    console.error('\nFAIL:');
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  console.log('\nPASS: flooder throttled to budget, compliant client unaffected');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
