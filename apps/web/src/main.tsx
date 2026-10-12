import { createRoot } from 'react-dom/client';
import { App } from './App';
import { applyTheme } from './theme';
import { initPanelWidths } from './components/ResizeHandle';
import { installCardArt } from './cardArt';
import './styles.css';

applyTheme();
initPanelWidths();
installCardArt();
createRoot(document.getElementById('root')!).render(<App />);
