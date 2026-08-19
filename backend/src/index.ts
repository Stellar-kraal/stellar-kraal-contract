import { createApp } from './app';
import { Server } from 'http';

const port = Number(process.env.PORT ?? 3001);
const { app, indexer, webhookDelivery } = createApp();

const server: Server = app.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`stellarkraal backend listening on :${port}`);

  // Start background services after the server is bound
  indexer.start();
  webhookDelivery.start();
});

let shuttingDown = false;

function gracefulShutdown(signal: string): void {
  if (shuttingDown) return;
  shuttingDown = true;

  // eslint-disable-next-line no-console
  console.log(`received ${signal}, starting graceful shutdown`);

  // Stop background work first so no new indexer ticks or webhook drains start.
  // In-flight ticks/drains are allowed to finish naturally.
  indexer.stop();
  webhookDelivery.stop();

  // Close the HTTP server: existing connections may finish, no new ones accepted.
  server.close((err) => {
    if (err) {
      // eslint-disable-next-line no-console
      console.error('error while closing http server:', err);
      process.exit(1);
    }
    // eslint-disable-next-line no-console
    console.log('http server closed, shutdown complete');
    process.exit(0);
  });

  // Force-terminate if graceful shutdown stalls (e.g. hung connections).
  setTimeout(() => {
    // eslint-disable-next-line no-console
    console.error('graceful shutdown timed out, forcing exit');
    process.exit(1);
  }, 30_000).unref();
}

process.on('SIGTERM', () => gracefulShutdown('SIGTERM'));
process.on('SIGINT', () => gracefulShutdown('SIGINT'));
