import { defineConfig } from "vitest/config";

// The reducer is pure, so the tests need no DOM. They live in `tests/`, mirroring
// the source tree, rather than beside the modules they cover.
export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
});
