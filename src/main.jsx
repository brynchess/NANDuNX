import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import Titlebar from './Titlebar.jsx';
import { LOCALE_STORAGE_KEY, initialLocale, locales, translate } from './i18n.js';
import './styles.css';

const isTauri = () => '__TAURI_INTERNALS__' in window;

function bytes(value) {
  if (!value) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / 1024 ** index).toFixed(index ? 1 : 0)} ${units[index]}`;
}

async function apiError(response, fallback) {
  const body = await response.json().catch(() => ({}));
  return body.error || fallback;
}

function uploadPartWithProgress(url, file, onProgress, t) {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    request.open('POST', url);
    request.responseType = 'json';
    request.setRequestHeader('Content-Type', 'application/octet-stream');
    request.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress(event.loaded, event.total);
    };
    request.onerror = () => reject(new Error(t('uploadInterrupted')));
    request.onload = () => {
      const body = request.response ?? (() => {
        try { return JSON.parse(request.responseText); } catch { return {}; }
      })();
      if (request.status < 200 || request.status >= 300) {
        reject(new Error(body?.error || t('uploadFailed')));
        return;
      }
      resolve(body);
    };
    request.send(file);
  });
}

function FileCard({ label, hint, kind, artifact, setArtifact, setError, t }) {
  const [uploadProgress, setUploadProgress] = useState(null);
  const [uploadId, setUploadId] = useState(null);

  useEffect(() => {
    if (!uploadId) return undefined;
    let stopped = false;
    let timer;
    async function poll() {
      try {
        const response = await fetch(`/api/v1/uploads/${uploadId}`);
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        const status = await response.json();
        if (!stopped) {
          setUploadProgress((current) => current && ({
            ...current,
            receivedBytes: status.received_bytes,
            receivedParts: status.received_parts,
            serverState: status.state,
          }));
          if (status.state === 'failed') setError(t('serverStoppedUpload'));
        }
      } catch (pollError) {
        if (!stopped) setError(t('readUploadProgressFailed', { error: String(pollError) }));
      }
      if (!stopped) timer = window.setTimeout(poll, 500);
    }
    poll();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [uploadId, setError, t]);

  async function selectDesktopFile() {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const path = await open({ multiple: false, directory: false });
      if (!path) return;
      const { invoke } = await import('@tauri-apps/api/core');
      setArtifact(await invoke('register_desktop_artifact', { path, kind }));
    } catch (error) {
      setError(t('filePickerFailed', { error: String(error) }));
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
      if (!start.ok) throw new Error(await apiError(start, t('genericRequestError')));
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
        }, t);
        sentBeforePart += file.size;
      }
      const complete = await fetch(`/api/v1/uploads/${session.id}/complete`, { method: 'POST' });
      if (!complete.ok) throw new Error(await apiError(complete, t('genericRequestError')));
      const receipt = await complete.json();
      setArtifact(receipt);
    } catch (error) {
      setError(t('uploadFailed', { error: String(error) }));
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
        <button type="button" className="secondary" onClick={selectDesktopFile}>{t('chooseFile')}</button>
      ) : (
        <input type="file" multiple={kind === 'backup'} onChange={uploadWebFile} disabled={Boolean(uploadProgress)} />
      )}
      {uploadProgress && (
        <span className="upload-progress" aria-live="polite">
          <progress value={uploadProgress.receivedBytes} max={uploadProgress.totalBytes || 1} />
          {t('serverSaved', { received: bytes(uploadProgress.receivedBytes), total: bytes(uploadProgress.totalBytes) })}
          <small>{t('browserSent', { sent: bytes(uploadProgress.sentBytes), current: uploadProgress.currentPart, count: uploadProgress.partCount, saved: uploadProgress.receivedParts })}</small>
        </span>
      )}
      <span className={artifact ? 'selected-file' : 'file-state'}>
        {artifact ? t(kind === 'backup' ? 'backupAdded' : 'fileAdded', { size: bytes(artifact.byte_len) }) : t('noFile')}
      </span>
    </label>
  );
}

function App() {
  const [locale, setLocale] = useState(initialLocale);
  const t = (key, values) => translate(locale, key, values);
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
  const modeName = inPlace ? t('existingUserExpansion') : mode === 'restore' ? t('plainRestore') : t('restoreAndExpand');
  const editionNames = { windows: 'Windows', linux: 'Linux', docker: 'Docker', web: 'Web' };
  const windowsDesktop = isTauri() && (runtime?.edition === 'windows' || (!runtime && navigator.platform.startsWith('Win')));
  const logEvent = (message, type = 'info') => setSessionLog((entries) => [...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString(locales[locale].locale), message, type }]);

  useEffect(() => {
    const root = document.getElementById('root');
    document.documentElement.lang = locale;
    try { window.localStorage.setItem(LOCALE_STORAGE_KEY, locale); } catch { /* Storage is optional. */ }
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
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        setDevices(await response.json());
      }
    } catch (loadError) {
      setError(t('devicesFailed', { error: String(loadError) }));
    } finally {
      setLoadingDevices(false);
    }
  }

  useEffect(() => { loadDevices(); }, [locale]);

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
          if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
          view = await response.json();
        }
        if (!disposed) setRestoreOperation(view);
      } catch (pollError) {
        setError(t('progressFailed', { error: String(pollError) }));
      }
    }, 750);
    return () => { disposed = true; window.clearInterval(poll); };
  }, [restoreOperation?.id, restoreOperation?.status?.state]);

  useEffect(() => {
    const status = restoreOperation?.status;
    if (!status) return;
    if (status.state === 'running') {
      const label = t(`phase_${status.progress.phase}`) === `phase_${status.progress.phase}` ? status.progress.phase : t(`phase_${status.progress.phase}`);
      setSessionLog((entries) => entries.at(-1)?.phase === status.progress.phase ? entries : [
        ...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString(locales[locale].locale), message: label, phase: status.progress.phase, type: 'phase' },
      ]);
    } else {
      setSessionLog((entries) => entries.at(-1)?.result === status.state ? entries : [
        ...entries.slice(-99), { id: crypto.randomUUID(), time: new Date().toLocaleTimeString(locales[locale].locale), message: status.state === 'failed' ? t('operationFailed') : status.report.status === 'completed' ? t('writeVerified') : t('operationCanceled'), result: status.state, type: status.state === 'failed' ? 'error' : 'success' },
      ]);
    }
  }, [restoreOperation?.status?.state, restoreOperation?.status?.progress?.phase, locale]);

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
        ? t('chooseKeysetAndDevice')
        : keysetRequired
          ? t('chooseBackupKeysetDevice')
          : t('chooseBackupDevice'));
      return;
    }
    if (!inPlace && Boolean(boot0) !== Boolean(boot1)) {
      setPlanning(false);
      setError(t('bootPairRequired'));
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
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        nextPlan = await response.json();
      }
      setPlan(nextPlan);
      setStage(1);
      logEvent(t('planPrepared'));
    } catch (planError) {
      setError(String(planError));
      logEvent(t('planFailed'), 'error');
    } finally {
      setPlanning(false);
    }
  }

  async function runPreflight() {
    if (startingOperation || restoreOperation?.status?.state === 'running') return;
    setError('');
    setPreflight(null);
    setChecking(true);
    logEvent(t('preflightStarted'));
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
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        report = await response.json();
      }
      setPreflight(report);
      logEvent(t('preflightPassed'));
    } catch (preflightError) {
      setError(t('preflightError', { error: String(preflightError) }));
      logEvent(t('preflightFailed'), 'error');
    } finally {
      setChecking(false);
    }
  }

  async function startRestore() {
    setError('');
    if (!plan || !preflight || typedConfirmation !== plan.target.path) {
      setError(t('typeFullTarget'));
      return;
    }
    if (startingOperation || restoreOperation?.status?.state === 'running') return;
    setStartingOperation(true);
    setStage(2);
    logEvent(t('authorizationStarted'));
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
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        view = await response.json();
      }
      setRestoreOperation(view);
      logEvent(t('taskStarted'));
    } catch (restoreError) {
      setError(t('startFailed', { error: String(restoreError) }));
      logEvent(t('taskStartFailed'), 'error');
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
        if (!response.ok) throw new Error(await apiError(response, t('genericRequestError')));
        setRestoreOperation(await response.json());
      }
      logEvent(t('cancelRequestedLog'));
    } catch (cancelError) {
      setError(t('cancelFailed', { error: String(cancelError) }));
    }
  }

  return (
    <div className={windowsDesktop ? 'desktop-frame windows-desktop' : 'desktop-frame'}>
      {windowsDesktop && <Titlebar closeDisabled={busy} onError={setError} t={t} />}
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand"><img className="brand-mark" src="/nandunx-icon.png" alt="" /><span><strong>NANDuNX</strong><small>{t('appTagline')}</small></span></div>
        <div className="sidebar-label">{t('workflow')}</div>
        <nav className="steps" aria-label={t('workflow')}>
          <button type="button" className={stage === 0 ? 'step active' : 'step'} onClick={() => setStage(0)} disabled={running}><span>01</span><span>{t('preparation')}<small>{t('stepPreparation')}</small></span></button>
          <button type="button" className={stage === 1 ? 'step active' : 'step'} onClick={() => setStage(1)} disabled={!plan || running}><span>02</span><span>{t('planAndChecks')}<small>{t('stepPlan')}</small></span></button>
          <button type="button" className={stage === 2 ? 'step active' : 'step'} onClick={() => setStage(2)} disabled={!restoreOperation && !startingOperation}><span>03</span><span>{t('execution')}<small>{t('stepExecution')}</small></span></button>
        </nav>
        <div className="language-picker"><label htmlFor="language">{t('selectLanguage')}</label><select id="language" value={locale} onChange={(event) => setLocale(event.target.value)} disabled={running}>{Object.entries(locales).map(([code, details]) => <option key={code} value={code}>{details.label}</option>)}</select></div>
        <div className="sidebar-bottom"><span className="runtime-dot" />{runtime ? `${editionNames[runtime.edition] || 'Web'} · v${runtime.app_version}` : isTauri() ? `${t('desktop')} · ${t('checkingVersion')}` : `Web · ${t('checkingVersion')}`}<small>{t('engine')} {runtime?.engine_version || '—'}</small></div>
      </aside>
      <div className="workspace">
        <header className="topbar"><div><span className="eyebrow">{t('mediaOperation')}</span><h1>{stage === 0 ? t('preparation') : stage === 1 ? t('planAndChecks') : t('execution')}</h1><p>{stage === 0 ? t('preparationHint') : stage === 1 ? t('planHint') : t('executionHint')}</p></div><span className="edition-badge">{runtime ? `${editionNames[runtime.edition] || 'Web'} · v${runtime.app_version}` : isTauri() ? t('desktop') : 'Web'}</span></header>
        {error && <p className="error global-error" role="alert">{error}</p>}
        <div className="workspace-body"><div className="stage-content">
      {stage === 0 && <form className="operation" onSubmit={makePlan}>
        <fieldset className="operation-controls" disabled={busy}>
          <fieldset>
            <legend>{t('chooseVariant')}</legend>
            <label className="choice">
              <input type="radio" name="mode" value="restore" checked={mode === 'restore'} onChange={(event) => setMode(event.target.value)} />
              <span><strong>{t('plainRestore')}</strong><small>{t('plainRestoreHint')}</small></span>
            </label>
            <label className="choice">
              <input type="radio" name="mode" value="restore_and_expand_user" checked={mode === 'restore_and_expand_user'} onChange={(event) => setMode(event.target.value)} />
              <span><strong>{t('restoreAndExpand')}</strong><small>{t('restoreAndExpandHint')}</small></span>
            </label>
            <label className="choice">
              <input type="radio" name="mode" value="expand_user_in_place" checked={inPlace} onChange={(event) => setMode(event.target.value)} />
              <span><strong>{t('expandInPlace')}</strong><small>{t('expandInPlaceHint')}</small></span>
            </label>
          </fieldset>

          <fieldset>
            <legend>{t('addFiles')}</legend>
            <div className="files">
              {!inPlace && <FileCard label={t('backup')} hint={isTauri() ? t('backupDesktopHint') : t('backupWebHint')} kind="backup" artifact={backup} setArtifact={setBackup} setError={setError} t={t} />}
              <FileCard label={t('keyset')} hint={keysetRequired ? t('keysetRequiredHint') : t('keysetOptionalHint')} kind="keyset" artifact={keyset} setArtifact={setKeyset} setError={setError} t={t} />
              {!inPlace && <FileCard label={t('boot0')} hint={t('boot0Hint')} kind="boot0" artifact={boot0} setArtifact={setBoot0} setError={setError} t={t} />}
              {!inPlace && <FileCard label={t('boot1')} hint={t('boot1Hint')} kind="boot1" artifact={boot1} setArtifact={setBoot1} setError={setError} t={t} />}
            </div>
            <p className="help">{t('webUploadPrivacy')}</p>
          </fieldset>

          <fieldset>
            <legend>{t('chooseTarget')}</legend>
            <div className="target-row">
              <select value={targetPath} onChange={(event) => setTargetPath(event.target.value)} required>
                <option value="">{loadingDevices ? t('loadingDevices') : t('chooseBlockDevice')}</option>
                {devices.map((device) => (
                  <option key={device.path} value={device.path} disabled={device.is_read_only}>
                    {device.path} · {device.model} · {bytes(device.byte_len)}{device.is_read_only ? ` · ${t('readOnly')}` : ''}
                  </option>
                ))}
              </select>
              <button type="button" className="secondary" onClick={loadDevices} disabled={loadingDevices}>{t('refresh')}</button>
            </div>
            <p className="danger">{inPlace ? t('inPlaceDanger') : t('restoreDanger')}</p>
          </fieldset>

          <div className="form-footer"><span>{t('planReadOnly')}</span><button className="primary" type="submit" disabled={busy}>{planning ? t('preparing') : t('preparePlan')}</button></div>
        </fieldset>
      </form>}

      {stage === 1 && plan && !preflight && (
        <section className="plan" aria-live="polite">
          <p className="eyebrow">{t('planReady')}</p>
          <h2>{inPlace ? t('existingUserExpansion') : plan.mode === 'restore' ? t('plainRestore') : t('restoreAndExpand')}</h2>
          <dl>
            <div><dt>{t('target')}</dt><dd>{plan.target.path} · {bytes(plan.target.byte_len)}</dd></div>
            {!inPlace && <div><dt>{t('sourceBackup')}</dt><dd>{bytes(plan.backup.byte_len)}</dd></div>}
            {(plan.boot0 || plan.boot1) && <div><dt>{t('boot')}</dt><dd>{t('bootPairAdded')}</dd></div>}
            <div><dt>{t('status')}</dt><dd>{t('nextReadOnlyCheck')}</dd></div>
          </dl>
          {!inPlace && !plan.target.boot_partitions_available && (
            <p className="warning"><strong>{t('bootUnavailableTitle')}</strong> {t('bootUnavailable')}</p>
          )}
          <button type="button" className="secondary preflight-button" onClick={runPreflight} disabled={busy}>{checking ? t('checking') : t('checkFilesAndDevice')}</button>
        </section>
      )}

      {stage === 1 && preflight && (
        <section className="plan preflight" aria-live="polite">
          <p className="eyebrow">{t('preflightComplete')}</p>
          <h2>{t('validFilesAndUser')}</h2>
          <p className="target-summary">{t('targetDevice')} <strong>{plan.target.path}</strong> · {bytes(plan.target.byte_len)}</p>
          {!inPlace && !plan.target.boot_partitions_available && <p className="warning">{t('adapterBootUnavailable')}</p>}
          <dl>
            {!inPlace && <><div><dt>{t('sourceFormat')}</dt><dd>{preflight.source.format === 'full_nand' ? 'FULL NAND (BOOT0 + BOOT1 + RAWNAND)' : 'RAWNAND'}</dd></div><div><dt>{t('sourceImage')}</dt><dd>{bytes(preflight.source.image_byte_len)} RAWNAND · backup GPT LBA {preflight.source.backup_gpt_lba}</dd></div><div><dt>{t('backupParts')}</dt><dd>{preflight.source.source_part_count}</dd></div><div><dt>BOOT0 / BOOT1</dt><dd>{preflight.source.boot0_source} / {preflight.source.boot1_source}</dd></div><div><dt>{t('gptPartitions')}</dt><dd>{preflight.source.partitions.length}</dd></div></>}
            <div><dt>{t('user')}</dt><dd>LBA {(inPlace ? preflight.user_partition : preflight.source.user_partition).first_lba}–{(inPlace ? preflight.user_partition : preflight.source.user_partition).last_lba}</dd></div>
            <div><dt>{t('keysetUser')}</dt><dd>{inPlace || preflight.keyset_validated_against_user_fat32 ? t('keysetValidated') : t('plainNoKeyValidation')}</dd></div>
            {preflight.user_fat32 && <div><dt>{t('fat32User')}</dt><dd>{t('fatStats', { allocated: preflight.user_fat32.allocated_clusters, free: preflight.user_fat32.free_clusters, bad: preflight.user_fat32.bad_clusters, chains: preflight.user_fat32.chain_count })}</dd></div>}
            {preflight.expanded_user && <div><dt>{t('expandedUser')}</dt><dd>{bytes(preflight.expanded_user.target_user_sectors * 512)} · +{bytes(preflight.expanded_user.gained_sectors * 512)}</dd></div>}
            {preflight.fat32_expansion && <div><dt>{t('fatGeometry')}</dt><dd>{t('fatGeometryValue', { current: preflight.fat32_expansion.current_sectors_per_fat, target: preflight.fat32_expansion.target_sectors_per_fat, shift: preflight.fat32_expansion.data_start_shift_sectors })}</dd></div>}
            {inPlace && <div><dt>{t('relocations')}</dt><dd>{t('relocationValue', { count: preflight.relocation_count })}</dd></div>}
          </dl>
          <p className="help">{inPlace ? t('inPlaceChecked') : t('restoreChecked')}</p>
          {(inPlace || (!plan.boot0 && !plan.boot1)) && (
            <div className="restore-action">
              <label>
                {t('confirmTarget')} <code>{plan.target.path}</code>
                <input value={typedConfirmation} disabled={startingOperation || restoreOperation?.status?.state === 'running'} onChange={(event) => setTypedConfirmation(event.target.value)} placeholder={plan.target.path} autoComplete="off" spellCheck="false" />
              </label>
              <button type="button" className="primary" onClick={startRestore} disabled={startingOperation || restoreOperation?.status?.state === 'running' || typedConfirmation !== plan.target.path}>{inPlace ? t('startExpand') : mode === 'restore' ? t('startPlainRestore') : t('startRestoreExpand')}</button>
            </div>
          )}
          {(plan.boot0 || plan.boot1) && (
            <p className="warning"><strong>{t('noBootWritesTitle')}</strong> {t('noBootWrites')}</p>
          )}
        </section>
      )}
        {stage === 2 && <section className="execution-card">
          <div className="section-heading"><span className="eyebrow">{t('task')}</span><h2>{modeName}</h2></div>
          <p className="help">{startingOperation ? t('recheckingTarget') : restoreOperation ? t('targetValue', { target: plan?.target.path }) : t('taskNotStarted')}</p>
          {restoreOperation?.status?.state === 'running' && <div className="execution-progress"><div className="progress-head"><strong>{t(`phase_${restoreOperation.status.progress.phase}`) === `phase_${restoreOperation.status.progress.phase}` ? t('preparation') : t(`phase_${restoreOperation.status.progress.phase}`)}</strong><span>{restoreOperation.status.progress.total_bytes ? `${Math.round(restoreOperation.status.progress.processed_bytes / restoreOperation.status.progress.total_bytes * 100)}%` : '—'}</span></div><progress value={restoreOperation.status.progress.processed_bytes} max={restoreOperation.status.progress.total_bytes || 1} /><span>{restoreOperation.status.progress.total_bytes ? t('from', { processed: bytes(restoreOperation.status.progress.processed_bytes), total: bytes(restoreOperation.status.progress.total_bytes) }) : t('waitingProgress')}</span></div>}
          {restoreOperation?.status?.state === 'finished' && <div className="result success"><strong>{restoreOperation.status.report.status === 'completed' ? t('writeVerified') : t('canceledSafely')}</strong>{restoreOperation.status.report.raw_nand_bytes !== undefined && <span>RAWNAND: {bytes(restoreOperation.status.report.raw_nand_bytes)} · {t('verifiedBytes', { value: bytes(restoreOperation.status.report.verified_bytes) })}</span>}{restoreOperation.status.report.target_user_sectors !== undefined && <span>USER: {bytes(restoreOperation.status.report.current_user_sectors * 512)} → {bytes(restoreOperation.status.report.target_user_sectors * 512)}</span>}</div>}
          {restoreOperation?.status?.state === 'failed' && <div className="result failure"><strong>{t('taskStopped')}</strong><span>{restoreOperation.status.error}</span></div>}
          {running && <div className="cancel-block"><button type="button" className="cancel-button" onClick={cancelRestore} disabled={restoreOperation.cancel_requested}>{restoreOperation.cancel_requested ? t('cancelRequested') : t('cancelTask')}</button><small>{t('cancellationSafety')}</small></div>}
          {isTauri() && running && <p className="help">{t('doNotClose')}</p>}
        </section>}
        </div>
        <aside className="job-panel" aria-label={t('currentTask')}>
          <div className="job-header"><span className="eyebrow">{t('currentTask')}</span><span className={running ? 'status-pill live' : 'status-pill'}>{running ? t('running') : restoreOperation?.status?.state === 'finished' ? t('completed') : restoreOperation?.status?.state === 'failed' ? t('failed') : t('idle')}</span></div>
          <h2>{restoreOperation || startingOperation ? modeName : t('noActiveTask')}</h2>
          <p className="job-summary">{running ? (t(`phase_${restoreOperation.status.progress.phase}`) === `phase_${restoreOperation.status.progress.phase}` ? t('preparing') : t(`phase_${restoreOperation.status.progress.phase}`)) : startingOperation ? t('targetCheck') : restoreOperation ? t('openExecution') : t('taskStatusHint')}</p>
          {running && <><progress className="job-progress" value={restoreOperation.status.progress.processed_bytes} max={restoreOperation.status.progress.total_bytes || 1} /><button type="button" className="cancel-button" onClick={cancelRestore} disabled={restoreOperation.cancel_requested}>{restoreOperation.cancel_requested ? t('cancelRequested') : t('cancelTask')}</button></>}
          <div className="log-header"><h3>{t('sessionLog')}</h3><span>{t('logEntries', { count: sessionLog.length })}</span></div>
          <div className="log-list" role="log" aria-label={t('sessionLog')} aria-live="polite">{sessionLog.length ? sessionLog.map((entry) => <div className={`log-entry ${entry.type}`} key={entry.id}><time>{entry.time}</time><span>{entry.message}</span></div>) : <p className="empty-log">{t('emptyLog')}</p>}</div>
        </aside></div>
      </div>
    </main>
    </div>
  );
}

createRoot(document.getElementById('root')).render(<App />);
