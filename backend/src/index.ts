import { createApp } from './app';

const port = Number(process.env.PORT ?? 3001);
const { app } = createApp();

app.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`stellarkraal backend listening on :${port}`);
});
