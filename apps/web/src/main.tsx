import { createRoot } from 'react-dom/client';
import { App } from './App';
import { applyTheme } from './theme';
import './styles.css';

applyTheme();
createRoot(document.getElementById('root')!).render(<App />);
