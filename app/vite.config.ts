// Vite builds the web page (src/) into dist/, which Tauri embeds in the app.
import { defineConfig } from "vite";

export default defineConfig({
  // Tauri expects a fixed port in dev (see devUrl in src-tauri/tauri.conf.json).
  server: { port: 1420, strictPort: true },
  // Keep Rust compiler errors visible in the terminal.
  clearScreen: false,
  build: { target: "safari15", outDir: "dist", emptyOutDir: true },
});
