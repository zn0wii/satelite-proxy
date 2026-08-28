import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import wgslVitePlugin from "@vgpu/wgsl/loader-vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), wgslVitePlugin()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },

  build: {
    // three.js 已 tree-shaken 仍约 530 kB(WebGLRenderer 是硬底),且
    // ParticleSphere 走 React.lazy 按需分包,仅仪表盘挂载时才加载解析。
    // Tauri 资源由本地磁盘提供,500 kB 的网络告警阈值不适用,放宽到 1024。
    chunkSizeWarningLimit: 1024,
  },
}));
