import React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

export default function Titlebar({ closeDisabled, onError }) {
  async function control(action) {
    try {
      await getCurrentWindow()[action]();
    } catch (error) {
      onError(`Nie udało się sterować oknem: ${String(error)}`);
    }
  }

  return (
    <div className="titlebar" aria-label="Pasek okna NANDuNX">
      <div className="titlebar-drag" data-tauri-drag-region>
        <img className="titlebar-icon" src="/nandunx-icon.png" alt="" data-tauri-drag-region />
        <span data-tauri-drag-region>NANDuNX</span>
      </div>
      <div className="titlebar-controls">
        <button type="button" aria-label="Minimalizuj okno" title="Minimalizuj" onClick={() => control('minimize')}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 11.5h10" /></svg>
        </button>
        <button type="button" aria-label="Maksymalizuj lub przywróć okno" title="Maksymalizuj lub przywróć" onClick={() => control('toggleMaximize')}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="3.5" y="3.5" width="9" height="9" rx=".5" /></svg>
        </button>
        <button type="button" className="titlebar-close" aria-label="Zamknij okno" title={closeDisabled ? 'Poczekaj na zakończenie zadania' : 'Zamknij'} onClick={() => control('close')} disabled={closeDisabled}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </div>
    </div>
  );
}
