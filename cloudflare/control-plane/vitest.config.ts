process.env.THALA_SHARED_AUTH_TOKEN ??= "dev-token";
process.env.THALA_GITHUB_TOKEN ??= "test-token";

import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.test.jsonc" },
    }),
  ],
  test: {
    deps: {
      optimizer: {
        ssr: {
          include: ["@cloudflare/sandbox", "@cloudflare/containers"],
        },
      },
    },
  },
});
