process.env.THALA_SHARED_AUTH_TOKEN ??= "dev-token";
process.env.THALA_GITHUB_TOKEN ??= "test-token";

import { defineWorkersConfig } from "@cloudflare/vitest-pool-workers/config";

export default defineWorkersConfig({
  test: {
    deps: {
      optimizer: {
        ssr: {
          include: ["@cloudflare/sandbox", "@cloudflare/containers"],
        },
      },
    },
    poolOptions: {
      workers: {
        isolatedStorage: false,
        wrangler: { configPath: "./wrangler.test.jsonc" },
      },
    },
  },
});
