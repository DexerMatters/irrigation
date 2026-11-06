import { defineConfig } from "vitest/config";

// The reducer is pure, so the tests need no DOM.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
