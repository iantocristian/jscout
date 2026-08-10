import { JsonCredentialStore } from "../src/registry.mjs";

const [file, provider] = process.argv.slice(2);
const store = new JsonCredentialStore(file);
await store.modify(provider, async () => {
  await new Promise((resolve) => setTimeout(resolve, 100));
  return { type: "api_key", key: provider };
});
