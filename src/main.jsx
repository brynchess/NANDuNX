import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import Titlebar from './Titlebar.jsx';
import { LOCALE_STORAGE_KEY, initialLocale, locales, localizeLegacyContent } from './i18n.js';
import './styles.css';

const isTauri = () => '__TAURI_INTERNALS__' in window;

function bytes(value) {
  if (!value) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / 1024 ** index).toFixed(index ? 1 : 0)} ${units[index]}`;
}

async function apiError(response) {
  const body = await response.json().catch(() => ({}));
  return body.error || 'Nie udało się wykonać żądania.';
}

const phaseLabels = {
  copying_payload: 'Kopiowanie obrazu',
  verifying_payload: 'Weryfikacja obrazu',
  committing_primary_metadata: 'Zapis primary GPT',
  verifying_primary_metadata: 'Weryfikacja primary GPT',
  relocating_user: 'Relokacja danych USER',
  writing_fat_mirrors: 'Zapis obu kopii FAT',
  committing_backup_gpt: 'Zapis backup GPT',
  committing_primary_gpt: 'Zapis primary GPT',
  committing_backup_boot_sector: 'Zapis zapasowego boot sectora',
  committing_primary_boot_sector: 'Zapis głównego boot sectora',
  verifying_expanded_user: 'Weryfikacja USER',
};

function uploadPartWithProgress(url, file, onProgress) {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    request.open('POST', url);
    request.responseType = 'json';
    request.setRequestHeader('Content-Type', 'application/octet-stream');
    request.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress(event.loaded, event.total);
    };
    request.onerror = () => reject(new Error('Przesyłanie pliku zostało przerwane.'));
    request.onload = () => {
      const body = request.response ?? (() => {
        try { return JSON.parse(request.responseText); } catch { return {}; }
      })();
      if (request.status < 200 || request.status >= 300) {
        reject(new Error(body?.error || 'Nie udało się przesłać pliku.'));
        return;
      }
      resolve(body);
    };
    request.send(file);
  });
}

function FileCard({ label, hint, kind, artifact, setArtifact, setError }) {
  const [uploadProgress, setUploadProgress] = useState(null);
  const [uploadId, setUploadId] = useState(null);

  useEffect(() => {
    if (!uploadId) return undefined;
    let stopped = false;
    let timer;
    async function poll() {
      try {
        const response = await fetch(`/api/v1/uploads/${uploadId}`);
        if (!response.ok) throw new Error(await apiError(response));
        const status = await response.json();
        if (!stopped) {
          setUploadProgress((current) => current && ({
            ...current,
            receivedBytes: status.received_bytes,
            receivedParts: status.received_parts,
            serverState: status.state,
          }));
          if (status.state === 'failed') setError('Serwer przerwał upload; wybierz pliki ponownie.');
        }
      } catch (pollError) {
        if (!stopped) setError(`Nie udało się odczytać postępu uploadu: ${String(pollError)}`);
      }
      if (!stopped) timer = window.setTimeout(poll, 500);
    }
    poll();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [uploadId, setError]);

  async function selectDesktopFile() {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const path = await open({ multiple: false, directory: false });
      if (!path) return;
      const { invoke } = await import('@tauri-apps/api/core');
      setArtifact(await invoke('register_desktop_artifact', { path, kind }));
    } catch (error) {
      setError(String(error));
    }
  }

  async function uploadWebFile(event) {
    const files = Array.from(event.target.files ?? []);
    if (!files.length) return;
    if (kind === 'backup') {
      files.sort((left, right) => left.name.localeCompare(right.name, undefined, { numeric: true, sensitivity: 'base' }));
    }
    try {
      const totalBytes = files.reduce((total, file) => total + file.size, 0);
      const start = await fetch('/api/v1/uploads', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ kind, partCount: files.length, totalBytes }),
      });
      if (!start.ok) throw new Error(await apiError(start));
      const session = await start.json();
      setUploadId(session.id);
      setUploadProgress({
        totalBytes,
        sentBytes: 0,
        receivedBytes: 0,
        currentPart: 0,
        partCount: files.length,
        receivedParts: 0,
        serverState: 'receiving',
      });

      let sentBeforePart = 0;
      for (const [part, file] of files.entries()) {
        setUploadProgress((current) => ({ ...current, currentPart: part + 1, sentBytes: sentBeforePart }));
        await uploadPartWithProgress(`/api/v1/uploads/${session.id}/parts/${part}`, file, (loaded) => {
          setUploadProgress((current) => current && ({
            ...current,
            currentPart: part + 1,
            sentBytes: sentBeforePart + loaded,
          }));
        });
        sentBeforePart += file.size;
      }
      const complete = await fetch(`/api/v1/uploads/${session.id}/complete`, { method: 'POST' });
      if (!complete.ok) throw new Error(await apiError(complete));
      const receipt = await complete.json();
      setArtifact(receipt);
    } catch (error) {
      setError(String(error));
    } finally {
      setUploadId(null);
      setUploadProgress(null);
      event.target.value = '';
    }
  }

  return (
    <label className="file-card">
      <span className="file-label">{label}</span>
      <span className="file-hint">{hint}</span>
      {isTauri() ? (
        <button type="button" className="secondary" onClick={selectDesktopFile}>Wybierz plik</button>
      ) : (
        <input type="file" multiple={kind === 'backup'} onChange={uploadWebFile} disabled={Boolean(uploadProgress)} />
      )}
      {uploadProgress && (
        <span className="upload-progress" aria-live="polite">
          <progress value={uploadProgress.receivedBytes} max={uploadProgress.totalBytes || 1} />
          Zapisano na serwerze: {bytes(uploadProgress.receivedBytes)} / {bytes(uploadProgress.totalBytes)}
          <small>Przeglądarka wysłała: {bytes(uploadProgress.sentBytes)} · plik {uploadProgress.currentPart} z {uploadProgress.partCount} · zapisane części: {uploadProgress.receivedParts}</small>
        </span>
      )}
      <span className={artifact ? 'selected-file' : 'file-state'}>
        {artifact ? `${kind === 'backup' ? 'Backup' : 'Plik'} dodany do sesji · ${bytes(artifact.byte_len)}` : 'Nie wybrano pliku'}
      </span>
    </label>
  );
}

function App() {
  const [locale, setLocale] = useState(initialLocale);
  const [mode, setMode] = useState('restore_and_expand_user');
  const [stage, setStage] = useState(0);
  const [planning, setPlanning] = useState(false);
  const [checking, setChecking] = useState(false);
  const [runtime, setRuntime] = useState(null);
  const [sessionLog, setSessionLog] = useState([]);
  const [backup, setBackup] = useState(null);
  const [keyset, setKeyset] = useState(null);
  const [boot0, setBoot0] = useState(null);
  const [boot1, setBoot1] = useState(null);
  const [devices, setDevices] = useState([]);
  const [targetPath, setTargetPath] = useState('');
  const [plan, setPlan] = useState(null);
  const [preflight, setPreflight] = useState(null);
  const [typedConfirmation, setTypedConfirmation] = useState('');
  const [restoreOperation, setRestoreOperation] = useState(null);
  const [startingOperation, setStartingOperation] = useState(false);
  const [error, setError] = useState('');
  const [loadingDevices, setLoadingDevices] = useState(true);
  const inPlace = mode === 'expand_user_in_place';
  const keysetRequired = mode === 'restore_and_expand_user' || inPlace;
  const running = restoreOperation?.status?.state === 'running';
  const busy = running || startingOperation || planning || checking;
  const modeName = inPlace ? 'Rozszerzenie USER' : mode === 'restore' ? 'Przywrócenie backupu' : 'Przywrócenie i rozszerzenie';
  const editionNames = { windows: 'Windows', linux: 'Linux', docker: 'Docker', web: 'Web' };
  const windowsDesktop = isTauri() && (runtime?.edition === 'windows' || (!runtime && navigator.platform.startsWith('Win')));
  const logEvent = (message, type = 'info') => setSessionLog((entries) => [...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString('pl-PL'), message, type }]);

  useEffect(() => {
    const root = document.getElementById('root');
    document.documentElement.lang = locale;
    try { window.localStorage.setItem(LOCALE_STORAGE_KEY, locale); } catch { /* Storage is optional. */ }
    localizeLegacyContent(root, locale);
    const observer = new MutationObserver(() => localizeLegacyContent(root, locale));
    observer.observe(root, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, [locale]);

  useEffect(() => {
    async function loadRuntime() {
      try {
        if (isTauri()) {
          const { invoke } = await import('@tauri-apps/api/core');
          setRuntime(await invoke('status'));
        } else {
          const response = await fetch('/api/v1/status');
          if (response.ok) setRuntime(await response.json());
        }
      } catch { /* Podgląd Vite może działać bez backendu. */ }
    }
    loadRuntime();
  }, []);

  useEffect(() => {
    document.documentElement.classList.toggle('windows-mica', windowsDesktop && runtime?.mica_enabled === true);
    return () => document.documentElement.classList.remove('windows-mica');
  }, [windowsDesktop, runtime?.mica_enabled]);


  async function loadDevices() {
    setLoadingDevices(true);
    setError('');
    try {
      if (isTauri()) {
        const { invoke } = await import('@tauri-apps/api/core');
        setDevices(await invoke('list_desktop_block_devices'));
      } else {
        const response = await fetch('/api/v1/devices');
        if (!response.ok) throw new Error(await apiError(response));
        setDevices(await response.json());
      }
    } catch (loadError) {
      setError(`Nie udało się pobrać urządzeń: ${String(loadError)}`);
    } finally {
      setLoadingDevices(false);
    }
  }

  useEffect(() => { loadDevices(); }, []);

  useEffect(() => {
    if (!running) return undefined;
    const warnBeforeUnload = (event) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warnBeforeUnload);
    return () => window.removeEventListener('beforeunload', warnBeforeUnload);
  }, [running]);

  useEffect(() => {
    setPlan(null);
    setPreflight(null);
    setTypedConfirmation('');
    setRestoreOperation(null);
    setStage(0);
  }, [mode, targetPath, backup?.id, keyset?.id, boot0?.id, boot1?.id]);

  useEffect(() => {
    if (restoreOperation?.status?.state !== 'running') return undefined;
    let disposed = false;
    const poll = window.setInterval(async () => {
      try {
        let view;
        if (isTauri()) {
          const { invoke } = await import('@tauri-apps/api/core');
          view = await invoke('desktop_operation_status', { id: restoreOperation.id });
        } else {
          const response = await fetch(`/api/v1/operations/${restoreOperation.id}`);
          if (!response.ok) throw new Error(await apiError(response));
          view = await response.json();
        }
        if (!disposed) setRestoreOperation(view);
      } catch (pollError) {
        setError(`Nie udało się odczytać postępu: ${String(pollError)}`);
      }
    }, 750);
    return () => { disposed = true; window.clearInterval(poll); };
  }, [restoreOperation?.id, restoreOperation?.status?.state]);

  useEffect(() => {
    const status = restoreOperation?.status;
    if (!status) return;
    if (status.state === 'running') {
      const label = phaseLabels[status.progress.phase] || status.progress.phase;
      setSessionLog((entries) => entries.at(-1)?.phase === status.progress.phase ? entries : [
        ...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString('pl-PL'), message: label, phase: status.progress.phase, type: 'phase' },
      ]);
    } else {
      setSessionLog((entries) => entries.at(-1)?.result === status.state ? entries : [
        ...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString('pl-PL'), message: status.state === 'failed' ? 'Zadanie zakończyło się błędem. Sprawdź komunikat w kroku Wykonanie.' : status.report.status === 'completed' ? 'Zapis i weryfikacja zakończone.' : 'Zadanie anulowano.', result: status.state, type: status.state === 'failed' ? 'error' : 'success' },
      ]);
    }
  }, [restoreOperation?.status?.state, restoreOperation?.status?.progress?.phase]);

  async function makePlan(event) {
    event.preventDefault();
    if (startingOperation || restoreOperation?.status?.state === 'running') return;
    setError('');
    setPlanning(true);
    setPlan(null);
    setPreflight(null);
    setTypedConfirmation('');
    setRestoreOperation(null);
    if (!targetPath || (keysetRequired && !keyset) || (!inPlace && !backup)) {
      setPlanning(false);
      setError(inPlace
        ? 'Wybierz keyset oraz urządzenie z istniejącą partycją USER.'
        : keysetRequired
          ? 'Wybierz backup, keyset oraz docelowe urządzenie.'
          : 'Wybierz backup oraz docelowe urządzenie.');
      return;
    }
    if (!inPlace && Boolean(boot0) !== Boolean(boot1)) {
      setPlanning(false);
      setError('Dodaj BOOT0 i BOOT1 jako kompletną parę albo pozostaw oba pola puste.');
      return;
    }
    try {
      let nextPlan;
      if (isTauri()) {
        const { invoke } = await import('@tauri-apps/api/core');
        nextPlan = await invoke(inPlace ? 'plan_desktop_in_place_operation' : 'plan_desktop_operation', inPlace ? {
          keysetId: keyset.id,
          targetPath,
        } : {
          mode,
          backupId: backup.id,
          keysetId: keyset?.id ?? null,
          boot0Id: boot0?.id ?? null,
          boot1Id: boot1?.id ?? null,
          targetPath,
        });
      } else {
        const response = await fetch(inPlace ? '/api/v1/operations/in-place/plan' : '/api/v1/operations/plan', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(inPlace
            ? { keysetId: keyset.id, targetPath }
            : { mode, backupId: backup.id, keysetId: keyset?.id ?? null, boot0Id: boot0?.id ?? null, boot1Id: boot1?.id ?? null, targetPath }),
        });
        if (!response.ok) throw new Error(await apiError(response));
        nextPlan = await response.json();
      }
      setPlan(nextPlan);
      setStage(1);
      logEvent('Plan przygotowany. Sprawdź cel i uruchom kontrolę odczytową.');
    } catch (planError) {
      setError(String(planError));
      logEvent('Nie udało się przygotować planu.', 'error');
    } finally {
      setPlanning(false);
    }
  }

  async function runPreflight() {
    if (startingOperation || restoreOperation?.status?.state === 'running') return;
    setError('');
    setPreflight(null);
    setChecking(true);
    logEvent('Rozpoczęto kontrolę odczytową.');
    try {
      let report;
      if (isTauri()) {
        const { invoke } = await import('@tauri-apps/api/core');
        report = await invoke(inPlace ? 'preflight_desktop_in_place_operation' : 'preflight_desktop_operation', inPlace ? {
          keysetId: keyset.id,
          targetPath,
        } : {
          mode,
          backupId: backup.id,
          keysetId: keyset?.id ?? null,
          boot0Id: boot0?.id ?? null,
          boot1Id: boot1?.id ?? null,
          targetPath,
        });
      } else {
        const response = await fetch(inPlace ? '/api/v1/operations/in-place/preflight' : '/api/v1/operations/preflight', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(inPlace
            ? { keysetId: keyset.id, targetPath }
            : { mode, backupId: backup.id, keysetId: keyset?.id ?? null, boot0Id: boot0?.id ?? null, boot1Id: boot1?.id ?? null, targetPath }),
        });
        if (!response.ok) throw new Error(await apiError(response));
        report = await response.json();
      }
      setPreflight(report);
      logEvent('Kontrola odczytowa zakończona pomyślnie.');
    } catch (preflightError) {
      setError(`Kontrola nie powiodła się: ${String(preflightError)}`);
      logEvent('Kontrola odczytowa nie powiodła się.', 'error');
    } finally {
      setChecking(false);
    }
  }

  async function startRestore() {
    setError('');
    if (!plan || !preflight || typedConfirmation !== plan.target.path) {
      setError('Po udanym preflightcie wpisz dokładną, pełną ścieżkę urządzenia docelowego.');
      return;
    }
    if (startingOperation || restoreOperation?.status?.state === 'running') return;
    setStartingOperation(true);
    setStage(2);
    logEvent('Trwa ponowna kontrola celu i autoryzacja zapisu.');
    try {
      const request = inPlace ? { keysetId: keyset.id, targetPath, typedConfirmation } : {
        mode, backupId: backup.id, keysetId: keyset?.id ?? null,
        boot0Id: boot0?.id ?? null, boot1Id: boot1?.id ?? null, targetPath, typedConfirmation,
      };
      let view;
      if (isTauri()) {
        const { invoke } = await import('@tauri-apps/api/core');
        view = await invoke(inPlace ? 'start_desktop_in_place_expansion' : 'start_desktop_restore', request);
      } else {
        const response = await fetch(inPlace ? '/api/v1/operations/in-place' : '/api/v1/operations/restore', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(request),
        });
        if (!response.ok) throw new Error(await apiError(response));
        view = await response.json();
      }
      setRestoreOperation(view);
      logEvent('Zadanie uruchomione.');
    } catch (restoreError) {
      setError(`Nie udało się rozpocząć operacji: ${String(restoreError)}`);
      logEvent('Nie udało się rozpocząć zadania.', 'error');
    } finally {
      setStartingOperation(false);
    }
  }

  async function cancelRestore() {
    if (!restoreOperation) return;
    setError('');
    try {
      if (isTauri()) {
        const { invoke } = await import('@tauri-apps/api/core');
        setRestoreOperation(await invoke('cancel_desktop_operation', { id: restoreOperation.id }));
      } else {
        const response = await fetch(`/api/v1/operations/${restoreOperation.id}/cancel`, { method: 'POST' });
        if (!response.ok) throw new Error(await apiError(response));
        setRestoreOperation(await response.json());
      }
      logEvent('Zażądano anulowania. Aplikacja zatrzyma zapis na bezpiecznej granicy.');
    } catch (cancelError) {
      setError(`Nie udało się zażądać anulowania: ${String(cancelError)}`);
    }
  }

  return (
    <div className={windowsDesktop ? 'desktop-frame windows-desktop' : 'desktop-frame'}>
      {windowsDesktop && <Titlebar closeDisabled={busy} onError={setError} />}
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand"><img className="brand-mark" src="/nandunx-icon.png" alt="" /><span><strong>NANDuNX</strong><small>Przywracanie NAND</small></span></div>
        <div className="sidebar-label">PRZEBIEG</div>
        <nav className="steps" aria-label="Kroki operacji">
          <button type="button" className={stage === 0 ? 'step active' : 'step'} onClick={() => setStage(0)} disabled={running}><span>01</span><span>Przygotowanie<small>Tryb, pliki i cel</small></span></button>
          <button type="button" className={stage === 1 ? 'step active' : 'step'} onClick={() => setStage(1)} disabled={!plan || running}><span>02</span><span>Plan i kontrola<small>Sprawdź przed zapisem</small></span></button>
          <button type="button" className={stage === 2 ? 'step active' : 'step'} onClick={() => setStage(2)} disabled={!restoreOperation && !startingOperation}><span>03</span><span>Wykonanie<small>Postęp i wynik</small></span></button>
        </nav>
        <div className="language-picker"><label htmlFor="language">Language</label><select id="language" value={locale} onChange={(event) => setLocale(event.target.value)} disabled={running}>{Object.entries(locales).map(([code, details]) => <option key={code} value={code}>{details.label}</option>)}</select></div>
        <div className="sidebar-bottom"><span className="runtime-dot" />{runtime ? `${editionNames[runtime.edition] || 'Web'} · v${runtime.app_version}` : isTauri() ? 'Desktop · checking version' : 'Web · checking version'}<small>Engine {runtime?.engine_version || '—'}</small></div>
      </aside>
      <div className="workspace">
        <header className="topbar"><div><span className="eyebrow">OPERACJA NA NOŚNIKU</span><h1>{stage === 0 ? 'Przygotowanie' : stage === 1 ? 'Plan i kontrola' : 'Wykonanie'}</h1><p>{stage === 0 ? 'Wybierz sposób pracy, własne pliki i urządzenie docelowe.' : stage === 1 ? 'Sprawdź plan i wykonaj kontrolę odczytową przed zapisem.' : 'Tu zobaczysz postęp, wynik i możliwość anulowania.'}</p></div><span className="edition-badge">{runtime ? `${editionNames[runtime.edition] || 'Web'} · v${runtime.app_version}` : isTauri() ? 'Desktop' : 'Web'}</span></header>
        {error && <p className="error global-error" role="alert">{error}</p>}
        <div className="workspace-body"><div className="stage-content">
      {stage === 0 && <form className="operation" onSubmit={makePlan}>
        <fieldset className="operation-controls" disabled={busy}>
          <fieldset>
            <legend>1. Wybierz wariant</legend>
            <label className="choice">
              <input type="radio" name="mode" value="restore" checked={mode === 'restore'} onChange={(event) => setMode(event.target.value)} />
              <span><strong>Zwykłe przywrócenie</strong><small>Odtwarza backup do docelowego nośnika bez zmiany rozmiaru USER.</small></span>
            </label>
            <label className="choice">
              <input type="radio" name="mode" value="restore_and_expand_user" checked={mode === 'restore_and_expand_user'} onChange={(event) => setMode(event.target.value)} />
              <span><strong>Przywrócenie z rozszerzeniem USER</strong><small>USER zajmie dostępną przestrzeń na większym nośniku.</small></span>
            </label>
            <label className="choice">
              <input type="radio" name="mode" value="expand_user_in_place" checked={inPlace} onChange={(event) => setMode(event.target.value)} />
              <span><strong>Rozszerz istniejący USER in-place</strong><small>Powiększa USER na podłączonym nośniku. Wymaga osobnego backupu i poprawnego keysetu.</small></span>
            </label>
          </fieldset>

          <fieldset>
            <legend>2. Dodaj pliki do bieżącej sesji</legend>
            <div className="files">
              {!inPlace && <FileCard label="Backup NAND" hint={isTauri() ? 'RAWNAND, FULL NAND lub pierwszy plik backupu dzielonego (.00).' : 'RAWNAND/FULL NAND albo wszystkie części backupu (.00, .01, …) wybrane naraz.'} kind="backup" artifact={backup} setArtifact={setBackup} setError={setError} />}
              <FileCard label="prod.keys / keyset" hint={keysetRequired ? 'Wymagany do rozszerzenia USER; pozostaje tylko w tej sesji.' : 'Opcjonalny przy zwykłym odtworzeniu; niezbędny później do operacji na USER.'} kind="keyset" artifact={keyset} setArtifact={setKeyset} setError={setError} />
              {!inPlace && <FileCard label="BOOT0 (opcjonalnie)" hint="Dodaj razem z BOOT1 tylko dla RAWNAND; każdy plik musi mieć 4 MiB." kind="boot0" artifact={boot0} setArtifact={setBoot0} setError={setError} />}
              {!inPlace && <FileCard label="BOOT1 (opcjonalnie)" hint="Dodaj razem z BOOT0 tylko dla RAWNAND; obraz FULL NAND już zawiera oba obszary." kind="boot1" artifact={boot1} setArtifact={setBoot1} setError={setError} />}
            </div>
            <p className="help">W wersji WWW upload jest przechowywany wyłącznie w prywatnym katalogu tymczasowym procesu i usuwany po jego zakończeniu.</p>
          </fieldset>

          <fieldset>
            <legend>3. Wybierz nośnik docelowy</legend>
            <div className="target-row">
              <select value={targetPath} onChange={(event) => setTargetPath(event.target.value)} required>
                <option value="">{loadingDevices ? 'Odczytywanie urządzeń…' : 'Wybierz urządzenie blokowe'}</option>
                {devices.map((device) => (
                  <option key={device.path} value={device.path} disabled={device.is_read_only}>
                    {device.path} · {device.model} · {bytes(device.byte_len)}{device.is_read_only ? ' · tylko odczyt' : ''}
                  </option>
                ))}
              </select>
              <button type="button" className="secondary" onClick={loadDevices} disabled={loadingDevices}>Odśwież</button>
            </div>
            <p className="danger">{inPlace ? 'Ta operacja zmienia istniejący USER. Przed jej rozpoczęciem zachowaj niezależny backup i sprawdź, czy wybrano właściwy nośnik.' : 'Przywrócenie nadpisze cały wybrany nośnik. Przed rozpoczęciem sprawdź urządzenie i zachowaj własny backup.'}</p>
          </fieldset>

          <div className="form-footer"><span>Plan i kontrola nie zapisują danych na urządzeniu.</span><button className="primary" type="submit" disabled={busy}>{planning ? 'Przygotowywanie…' : 'Przygotuj plan →'}</button></div>
        </fieldset>
      </form>}

      {stage === 1 && plan && !preflight && (
        <section className="plan" aria-live="polite">
          <p className="eyebrow">Plan gotowy</p>
          <h2>{inPlace ? 'Rozszerzenie istniejącego USER in-place' : plan.mode === 'restore' ? 'Zwykłe przywrócenie' : 'Przywrócenie z rozszerzeniem USER'}</h2>
          <dl>
            <div><dt>Cel</dt><dd>{plan.target.path} · {bytes(plan.target.byte_len)}</dd></div>
            {!inPlace && <div><dt>Backup</dt><dd>{bytes(plan.backup.byte_len)}</dd></div>}
            {(plan.boot0 || plan.boot1) && <div><dt>BOOT</dt><dd>Dodano osobne pliki BOOT0 i BOOT1; zostaną sprawdzone podczas preflightu.</dd></div>}
            <div><dt>Stan</dt><dd>Następnie sprawdź pliki i urządzenie bez zapisu.</dd></div>
          </dl>
          {!inPlace && !plan.target.boot_partitions_available && (
            <p className="warning"><strong>BOOT0 / BOOT1 niedostępne przez adapter.</strong> Możesz kontynuować preflight i pracę z RAWNAND/USER, ale backup BOOT0 oraz BOOT1 trzeba będzie przywrócić osobno z poziomu Hekate.</p>
          )}
          <button type="button" className="secondary preflight-button" onClick={runPreflight} disabled={busy}>{checking ? 'Trwa kontrola…' : 'Sprawdź pliki i urządzenie'}</button>
        </section>
      )}

      {stage === 1 && preflight && (
        <section className="plan preflight" aria-live="polite">
          <p className="eyebrow">Kontrola odczytowa zakończona</p>
          <h2>Pliki i układ USER są poprawne</h2>
          <p className="target-summary">Nośnik docelowy: <strong>{plan.target.path}</strong> · {bytes(plan.target.byte_len)}</p>
          {!inPlace && !plan.target.boot_partitions_available && <p className="warning">Adapter nie udostępnia BOOT0 i BOOT1. Po zakończeniu przywróć je osobno przez Hekate.</p>}
          <dl>
            {!inPlace && <><div><dt>Format źródła</dt><dd>{preflight.source.format === 'full_nand' ? 'FULL NAND (BOOT0 + BOOT1 + RAWNAND)' : 'RAWNAND'}</dd></div><div><dt>Obraz źródłowy</dt><dd>{bytes(preflight.source.image_byte_len)} RAWNAND · backup GPT LBA {preflight.source.backup_gpt_lba}</dd></div><div><dt>Części backupu</dt><dd>{preflight.source.source_part_count}</dd></div><div><dt>BOOT0 / BOOT1</dt><dd>{preflight.source.boot0_source} / {preflight.source.boot1_source}</dd></div><div><dt>Partycje GPT</dt><dd>{preflight.source.partitions.length}</dd></div></>}
            <div><dt>USER</dt><dd>LBA {(inPlace ? preflight.user_partition : preflight.source.user_partition).first_lba}–{(inPlace ? preflight.user_partition : preflight.source.user_partition).last_lba}</dd></div>
            <div><dt>Keyset / USER</dt><dd>{inPlace || preflight.keyset_validated_against_user_fat32 ? 'Potwierdzony przez zaszyfrowany boot sector FAT32.' : 'Zwykłe odtworzenie nie wymaga walidacji klucza USER.'}</dd></div>
            {preflight.user_fat32 && <div><dt>FAT32 USER</dt><dd>{preflight.user_fat32.allocated_clusters} zajętych · {preflight.user_fat32.free_clusters} wolnych · {preflight.user_fat32.bad_clusters} uszkodzonych klastrów · {preflight.user_fat32.chain_count} łańcuchów</dd></div>}
            {preflight.expanded_user && <div><dt>USER po rozszerzeniu</dt><dd>{bytes(preflight.expanded_user.target_user_sectors * 512)} · +{bytes(preflight.expanded_user.gained_sectors * 512)}</dd></div>}
            {preflight.fat32_expansion && <div><dt>Nowa geometria FAT32</dt><dd>FAT: {preflight.fat32_expansion.current_sectors_per_fat} → {preflight.fat32_expansion.target_sectors_per_fat} sektorów · początek danych: +{preflight.fat32_expansion.data_start_shift_sectors} sektorów</dd></div>}
            {inPlace && <div><dt>Relokacje</dt><dd>{preflight.relocation_count} zajętych klastrów zostanie odszyfrowanych i ponownie zaszyfrowanych.</dd></div>}
          </dl>
          <p className="help">{inPlace ? 'Sprawdzono układ partycji i dane USER bez zapisu na urządzeniu. Porównano obie kopie FAT i przygotowano plan przeniesienia danych.' : 'Sprawdzono układ backupu. Przy rozszerzaniu porównano obie kopie FAT i sprawdzono dane USER. Na urządzeniu docelowym niczego nie zapisano.'}</p>
          {(inPlace || (!plan.boot0 && !plan.boot1)) && (
            <div className="restore-action">
              <label>
                Potwierdź cel, wpisując <code>{plan.target.path}</code>
                <input value={typedConfirmation} disabled={startingOperation || restoreOperation?.status?.state === 'running'} onChange={(event) => setTypedConfirmation(event.target.value)} placeholder={plan.target.path} autoComplete="off" spellCheck="false" />
              </label>
              <button type="button" className="primary" onClick={startRestore} disabled={startingOperation || restoreOperation?.status?.state === 'running' || typedConfirmation !== plan.target.path}>{inPlace ? 'Rozpocznij rozszerzanie USER' : mode === 'restore' ? 'Rozpocznij zwykłe odtwarzanie' : 'Rozpocznij przywracanie i rozszerzanie'}</button>
            </div>
          )}
          {(plan.boot0 || plan.boot1) && (
            <p className="warning"><strong>Aplikacja nie zapisuje BOOT0 ani BOOT1.</strong> Odtwarzanie możesz uruchomić bez tej pary, a BOOT0 i BOOT1 przywrócić osobno przez Hekate.</p>
          )}
        </section>
      )}
        {stage === 2 && <section className="execution-card">
          <div className="section-heading"><span className="eyebrow">ZADANIE</span><h2>{modeName}</h2></div>
          <p className="help">{startingOperation ? 'Trwa ponowna kontrola celu i autoryzacja. Poczekaj na wynik.' : restoreOperation ? `Cel: ${plan?.target.path}` : 'Zadanie nie zostało jeszcze uruchomione.'}</p>
          {restoreOperation?.status?.state === 'running' && <div className="execution-progress"><div className="progress-head"><strong>{phaseLabels[restoreOperation.status.progress.phase] || 'Przygotowanie'}</strong><span>{restoreOperation.status.progress.total_bytes ? `${Math.round(restoreOperation.status.progress.processed_bytes / restoreOperation.status.progress.total_bytes * 100)}%` : '—'}</span></div><progress value={restoreOperation.status.progress.processed_bytes} max={restoreOperation.status.progress.total_bytes || 1} /><span>{restoreOperation.status.progress.total_bytes ? `${bytes(restoreOperation.status.progress.processed_bytes)} z ${bytes(restoreOperation.status.progress.total_bytes)}` : 'Oczekiwanie na pierwszy odczyt postępu…'}</span></div>}
          {restoreOperation?.status?.state === 'finished' && <div className="result success"><strong>{restoreOperation.status.report.status === 'completed' ? 'Zapis i weryfikacja zakończone.' : 'Operacja anulowana na bezpiecznej granicy.'}</strong>{restoreOperation.status.report.raw_nand_bytes !== undefined && <span>RAWNAND: {bytes(restoreOperation.status.report.raw_nand_bytes)} · zweryfikowano {bytes(restoreOperation.status.report.verified_bytes)}</span>}{restoreOperation.status.report.target_user_sectors !== undefined && <span>USER: {bytes(restoreOperation.status.report.current_user_sectors * 512)} → {bytes(restoreOperation.status.report.target_user_sectors * 512)}</span>}</div>}
          {restoreOperation?.status?.state === 'failed' && <div className="result failure"><strong>Zadanie zatrzymane</strong><span>{restoreOperation.status.error}</span></div>}
          {running && <div className="cancel-block"><button type="button" className="cancel-button" onClick={cancelRestore} disabled={restoreOperation.cancel_requested}>{restoreOperation.cancel_requested ? 'Zażądano anulowania' : 'Anuluj zadanie'}</button><small>Anulowanie następuje na bezpiecznej granicy przed końcowym commitem. Nie odłączaj nośnika, dopóki zadanie się nie zakończy.</small></div>}
          {isTauri() && running && <p className="help">Nie zamykaj okna podczas zapisu.</p>}
        </section>}
        </div>
        <aside className="job-panel" aria-label="Bieżące zadanie i dziennik sesji">
          <div className="job-header"><span className="eyebrow">BIEŻĄCE ZADANIE</span><span className={running ? 'status-pill live' : 'status-pill'}>{running ? 'W toku' : restoreOperation?.status?.state === 'finished' ? 'Zakończone' : restoreOperation?.status?.state === 'failed' ? 'Błąd' : 'Bezczynne'}</span></div>
          <h2>{restoreOperation || startingOperation ? modeName : 'Brak aktywnego zadania'}</h2>
          <p className="job-summary">{running ? phaseLabels[restoreOperation.status.progress.phase] || 'Przygotowanie…' : startingOperation ? 'Kontrola celu…' : restoreOperation ? 'Otwórz krok Wykonanie, aby zobaczyć wynik.' : 'Po uruchomieniu operacji zobaczysz tutaj jej stan.'}</p>
          {running && <><progress className="job-progress" value={restoreOperation.status.progress.processed_bytes} max={restoreOperation.status.progress.total_bytes || 1} /><button type="button" className="cancel-button" onClick={cancelRestore} disabled={restoreOperation.cancel_requested}>{restoreOperation.cancel_requested ? 'Oczekiwanie na przerwanie' : 'Anuluj zadanie'}</button></>}
          <div className="log-header"><h3>Dziennik sesji</h3><span>{sessionLog.length} wpisów</span></div>
          <div className="log-list" role="log" aria-label="Dziennik sesji" aria-live="polite">{sessionLog.length ? sessionLog.map((entry) => <div className={`log-entry ${entry.type}`} key={entry.id}><time>{entry.time}</time><span>{entry.message}</span></div>) : <p className="empty-log">Tu pojawią się zdarzenia z bieżącej sesji: kontrola, etapy zapisu i wynik.</p>}</div>
        </aside></div>
      </div>
    </main>
    </div>
  );
}

createRoot(document.getElementById('root')).render(<App />);
