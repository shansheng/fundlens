import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Tauri 期望相对 base path，以便打包进二进制资源
export default defineConfig({
  plugins: [react()],
  base: './',
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'es2022',
    outDir: 'dist',
    sourcemap: false,
  },
});
