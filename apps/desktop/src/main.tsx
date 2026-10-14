import {lazy} from 'react';
import {createRoot} from 'react-dom/client';
import DeferredView from './components/DeferredView.tsx';
import {PREVIEW_SURFACE} from './recognition.ts';
import './style.css';

// Each window loads only its own surface; the preview never mounts the application's controllers.
const App = lazy(() => import('./App.tsx'));
const RecognitionPreview = lazy(() => import('./pages/RecognitionPreview.tsx'));
const preview = new URLSearchParams(window.location.search).get('surface') === PREVIEW_SURFACE;
createRoot(document.getElementById('root')!).render(
  <DeferredView fallback={<progress aria-label="MadoMata"/>}
    error={cause => <section className="fault" role="alert"><strong>MadoMata</strong>
      <p>{cause instanceof Error ? cause.message : String(cause)}</p></section>}>
    {preview ? <RecognitionPreview/> : <App/>}
  </DeferredView>,
);
