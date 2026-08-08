import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `bun run dev` serves the UI with HMR and proxies the API to the Rust
// process, so editing a component is instant and the api keeps owning every
// byte of data. In production the api serves the built bundle from dist/ and
// this proxy is irrelevant.
export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 5174,
    proxy: { "/api": "http://127.0.0.1:7434" },
  },
  build: { outDir: "dist" },
});
