import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
// @ts-expect-error process is a nodejs global
// 1425 (not WAID's 1420) so both widgets can run dev side-by-side.
const port = Number(process.env.WHENCE_DEV_PORT) || 1425;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [tailwindcss(), sveltekit()],
  // Vite options tailored for Tauri development.
  clearScreen: false, // 1. don't obscure Rust errors
  server: {
    port, // 2. Tauri expects a fixed port
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1426 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"], // 3. don't watch the Rust tree
    },
  },
}));
