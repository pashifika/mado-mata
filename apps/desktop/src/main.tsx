import {createRoot} from 'react-dom/client';
import App from './App.tsx';
import RecognitionPreview from './pages/RecognitionPreview.tsx';
import {PREVIEW_SURFACE} from './recognition.ts';
import './style.css';

// The detached recognition preview window loads the same bundle with `?surface=recognition-preview`.
const preview = new URLSearchParams(window.location.search).get('surface') === PREVIEW_SURFACE;
createRoot(document.getElementById('root')!).render(preview ? <RecognitionPreview/> : <App/>);
