import { createRoot } from 'react-dom/client';
import { App } from './App';
import { applyTheme } from './theme';
import { initPanelWidths } from './components/ResizeHandle';
import { installCardArt } from './cardArt';
import { installShellEvents } from './shell';
import './styles.css';

applyTheme();
initPanelWidths();
installCardArt();
installShellEvents();
createRoot(document.getElementById('root')!).render(<App />);
