import {defineConfig} from 'vite';
import {trustedLibrariesPlugin} from './build/trusted-libraries.mjs';

export default defineConfig({
  plugins: [trustedLibrariesPlugin()],
  worker: {plugins: () => [trustedLibrariesPlugin()]},
});
