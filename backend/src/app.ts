import express, { Express } from 'express';
import { ChainClient, SimulatedChainClient } from './chain/chainClient';
import { AppConfig, loadConfig } from './config';
import { creditsRoutes } from './credits/routes';
import { Store } from './db/database';
import { marketplaceRoutes } from './marketplace/routes';

export interface AppDeps {
  store: Store;
  chain: ChainClient;
  config: AppConfig;
}

export interface App extends AppDeps {
  app: Express;
}

export function createApp(deps: Partial<AppDeps> = {}): App {
  const config = deps.config ?? loadConfig();
  const store = deps.store ?? new Store(process.env.DATABASE_PATH ?? 'stellarkraal.db');
  const chain = deps.chain ?? new SimulatedChainClient(store, config.now);

  const app = express();
  app.use(express.json());

  app.get('/health', (_req, res) => {
    res.json({ status: 'ok' });
  });

  const routeDeps = { store, chain, config };
  app.use('/marketplace', marketplaceRoutes(routeDeps));
  app.use('/credits', creditsRoutes(routeDeps));

  return { app, store, chain, config };
}
