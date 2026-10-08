export const DEFAULT_LOCALE = 'en';
export const LOCALE_STORAGE_KEY = 'nandunx.locale';

export const locales = {
  en: { label: 'English', locale: 'en-US' },
  pl: { label: 'Polski', locale: 'pl-PL' },
};

const en = {
  appTagline: 'NAND restore', workflow: 'WORKFLOW', mediaOperation: 'STORAGE OPERATION',
  preparation: 'Preparation', planAndChecks: 'Plan and checks', execution: 'Execution',
  preparationHint: 'Choose an operation, your files, and the target device.',
  planHint: 'Review the plan and run read-only checks before writing.',
  executionHint: 'View progress, results, and the option to cancel here.',
  stepPreparation: 'Mode, files and target', stepPlan: 'Review before writing', stepExecution: 'Progress and result',
  selectLanguage: 'Language', checkingVersion: 'checking version', engine: 'Engine', desktop: 'Desktop',
  chooseVariant: '1. Choose an operation', plainRestore: 'Plain restore',
  plainRestoreHint: 'Restores the backup to the target device without resizing USER.',
  restoreAndExpand: 'Restore and expand USER', restoreAndExpandHint: 'USER will use the available space on a larger device.',
  expandInPlace: 'Expand existing USER in place', expandInPlaceHint: 'Expands USER on the connected device. Requires a separate backup and a valid keyset.',
  addFiles: '2. Add files to this session', backup: 'NAND backup', keyset: 'prod.keys / keyset',
  backupDesktopHint: 'RAWNAND, FULL NAND, or the first split-backup file (.00).',
  backupWebHint: 'RAWNAND/FULL NAND, or all split-backup files (.00, .01, …) selected together.',
  keysetRequiredHint: 'Required to expand USER; retained only for this session.',
  keysetOptionalHint: 'Optional for plain restore; required later for USER operations.',
  boot0: 'BOOT0 (optional)', boot1: 'BOOT1 (optional)',
  boot0Hint: 'Add with BOOT1 only for RAWNAND; each file must be 4 MiB.',
  boot1Hint: 'Add with BOOT0 only for RAWNAND; FULL NAND already contains both areas.',
  webUploadPrivacy: 'In the Web edition, uploads are kept only in the process-private temporary directory and removed when it exits.',
  chooseTarget: '3. Choose the target device', loadingDevices: 'Reading devices…', chooseBlockDevice: 'Choose a block device', readOnly: 'read-only', refresh: 'Refresh',
  inPlaceDanger: 'This operation changes the existing USER. Keep an independent backup and verify the selected device before starting.',
  restoreDanger: 'Restore overwrites the whole selected device. Verify the device and keep your own backup before starting.',
  planReadOnly: 'Planning and checks do not write data to the device.', preparing: 'Preparing…', preparePlan: 'Prepare plan →',
  chooseFile: 'Choose file', serverSaved: 'Saved on server: {received} / {total}',
  browserSent: 'Browser sent: {sent} · file {current} of {count} · saved parts: {saved}',
  backupAdded: 'Backup added to session · {size}', fileAdded: 'File added to session · {size}', noFile: 'No file selected',
  planReady: 'Plan ready', existingUserExpansion: 'Expand existing USER in place', target: 'Target', sourceBackup: 'Backup', boot: 'BOOT', status: 'Status',
  bootPairAdded: 'Separate BOOT0 and BOOT1 files were added; they will be checked during preflight.',
  nextReadOnlyCheck: 'Next, check the files and device without writing.',
  bootUnavailableTitle: 'BOOT0 / BOOT1 unavailable through the adapter.',
  bootUnavailable: 'You can continue with preflight and RAWNAND/USER, but restore BOOT0 and BOOT1 separately with Hekate.',
  checking: 'Checking…', checkFilesAndDevice: 'Check files and device',
  preflightComplete: 'Read-only check complete', validFilesAndUser: 'Files and USER layout are valid', targetDevice: 'Target device:',
  adapterBootUnavailable: 'The adapter does not expose BOOT0 and BOOT1. Restore them separately with Hekate after completion.',
  sourceFormat: 'Source format', sourceImage: 'Source image', backupParts: 'Backup parts', gptPartitions: 'GPT partitions',
  user: 'USER', keysetUser: 'Keyset / USER', fat32User: 'USER FAT32', expandedUser: 'Expanded USER', fatGeometry: 'New FAT32 geometry', relocations: 'Relocations',
  keysetValidated: 'Validated against the encrypted FAT32 boot sector.', plainNoKeyValidation: 'Plain restore does not require USER-key validation.',
  fatStats: '{allocated} allocated · {free} free · {bad} bad clusters · {chains} chains',
  fatGeometryValue: 'FAT: {current} → {target} sectors · data start: +{shift} sectors',
  relocationValue: '{count} allocated clusters will be decrypted and encrypted again.',
  inPlaceChecked: 'The partition layout and USER data were checked without writing. Both FAT copies were compared and a data-relocation plan was prepared.',
  restoreChecked: 'The backup layout was checked. For expansion, both FAT copies and USER data were checked. Nothing was written to the target device.',
  confirmTarget: 'Confirm the target by typing', startExpand: 'Start USER expansion', startPlainRestore: 'Start plain restore', startRestoreExpand: 'Start restore and expansion',
  noBootWritesTitle: 'The application does not write BOOT0 or BOOT1.', noBootWrites: 'You can start restore without this pair and restore BOOT0 and BOOT1 separately with Hekate.',
  task: 'TASK', targetValue: 'Target: {target}', taskNotStarted: 'The task has not started yet.',
  recheckingTarget: 'The target is being checked and write authorization is being repeated. Wait for the result.',
  waitingProgress: 'Waiting for the first progress read…', from: '{processed} of {total}',
  writeVerified: 'Writing and verification complete.', verifiedBytes: 'verified {value}', canceledSafely: 'Operation canceled at a safe boundary.', taskStopped: 'Task stopped',
  cancelRequested: 'Cancellation requested', cancelTask: 'Cancel task', cancellationSafety: 'Cancellation occurs at a safe boundary before the final commit. Do not disconnect the device until the task has ended.',
  doNotClose: 'Do not close the window while writing.', currentTask: 'CURRENT TASK', running: 'Running', completed: 'Completed', failed: 'Failed', idle: 'Idle',
  noActiveTask: 'No active task', openExecution: 'Open Execution to view the result.', targetCheck: 'Checking target…', taskStatusHint: 'Once you start an operation, its status will appear here.',
  sessionLog: 'Session log', logEntries: '{count} entries', emptyLog: 'Events from this session will appear here: checks, write phases, and the result.',
  genericRequestError: 'The request could not be completed.', uploadInterrupted: 'File upload was interrupted.', uploadFailed: 'The file could not be uploaded.',
  serverStoppedUpload: 'The server stopped the upload; select the files again.', readUploadProgressFailed: 'Could not read upload progress: {error}',
  filePickerFailed: 'Could not select the file: {error}', devicesFailed: 'Could not load devices: {error}', progressFailed: 'Could not read progress: {error}',
  chooseKeysetAndDevice: 'Choose a keyset and a device with an existing USER partition.', chooseBackupKeysetDevice: 'Choose a backup, keyset, and target device.', chooseBackupDevice: 'Choose a backup and target device.',
  bootPairRequired: 'Add BOOT0 and BOOT1 as a complete pair, or leave both fields empty.', planPrepared: 'Plan prepared. Review the target and start the read-only check.', planFailed: 'Could not prepare the plan.',
  preflightStarted: 'Read-only check started.', preflightPassed: 'Read-only check completed successfully.', preflightFailed: 'Read-only check failed.', preflightError: 'Check failed: {error}',
  typeFullTarget: 'After a successful preflight, type the exact full target-device path.', authorizationStarted: 'Target recheck and write authorization started.',
  taskStarted: 'Task started.', startFailed: 'Could not start the operation: {error}', taskStartFailed: 'Could not start the task.', cancelRequestedLog: 'Cancellation requested. The application will stop writing at a safe boundary.', cancelFailed: 'Could not request cancellation: {error}',
  phase_copying_payload: 'Copying image', phase_verifying_payload: 'Verifying image', phase_committing_primary_metadata: 'Writing primary GPT', phase_verifying_primary_metadata: 'Verifying primary GPT', phase_relocating_user: 'Relocating USER data', phase_writing_fat_mirrors: 'Writing both FAT copies', phase_committing_backup_gpt: 'Writing backup GPT', phase_committing_primary_gpt: 'Writing primary GPT', phase_committing_backup_boot_sector: 'Writing backup boot sector', phase_committing_primary_boot_sector: 'Writing primary boot sector', phase_verifying_expanded_user: 'Verifying USER',
  operationFailed: 'The task ended with an error. Check the message in Execution.', operationCanceled: 'Task canceled.',
  titlebar: { window: 'NANDuNX window', minimize: 'Minimize window', maximize: 'Maximize or restore window', close: 'Close window', waitForTask: 'Wait for the task to finish', windowControlFailed: 'Could not control the window: {error}' },
};

const pl = {
  appTagline: 'Przywracanie NAND', workflow: 'PRZEBIEG', mediaOperation: 'OPERACJA NA NOŚNIKU', preparation: 'Przygotowanie', planAndChecks: 'Plan i kontrola', execution: 'Wykonanie', preparationHint: 'Wybierz sposób pracy, własne pliki i urządzenie docelowe.', planHint: 'Sprawdź plan i wykonaj kontrolę odczytową przed zapisem.', executionHint: 'Tu zobaczysz postęp, wynik i możliwość anulowania.', stepPreparation: 'Tryb, pliki i cel', stepPlan: 'Sprawdź przed zapisem', stepExecution: 'Postęp i wynik', selectLanguage: 'Język', checkingVersion: 'sprawdzanie wersji', engine: 'Silnik', desktop: 'Desktop',
  chooseVariant: '1. Wybierz wariant', plainRestore: 'Zwykłe przywrócenie', plainRestoreHint: 'Odtwarza backup do docelowego nośnika bez zmiany rozmiaru USER.', restoreAndExpand: 'Przywrócenie z rozszerzeniem USER', restoreAndExpandHint: 'USER zajmie dostępną przestrzeń na większym nośniku.', expandInPlace: 'Rozszerz istniejący USER in-place', expandInPlaceHint: 'Powiększa USER na podłączonym nośniku. Wymaga osobnego backupu i poprawnego keysetu.', addFiles: '2. Dodaj pliki do bieżącej sesji', backup: 'Backup NAND', keyset: 'prod.keys / keyset', backupDesktopHint: 'RAWNAND, FULL NAND lub pierwszy plik backupu dzielonego (.00).', backupWebHint: 'RAWNAND/FULL NAND albo wszystkie części backupu (.00, .01, …) wybrane naraz.', keysetRequiredHint: 'Wymagany do rozszerzenia USER; pozostaje tylko w tej sesji.', keysetOptionalHint: 'Opcjonalny przy zwykłym odtworzeniu; niezbędny później do operacji na USER.', boot0: 'BOOT0 (opcjonalnie)', boot1: 'BOOT1 (opcjonalnie)', boot0Hint: 'Dodaj razem z BOOT1 tylko dla RAWNAND; każdy plik musi mieć 4 MiB.', boot1Hint: 'Dodaj razem z BOOT0 tylko dla RAWNAND; obraz FULL NAND już zawiera oba obszary.', webUploadPrivacy: 'W wersji WWW upload jest przechowywany wyłącznie w prywatnym katalogu tymczasowym procesu i usuwany po jego zakończeniu.', chooseTarget: '3. Wybierz nośnik docelowy', loadingDevices: 'Odczytywanie urządzeń…', chooseBlockDevice: 'Wybierz urządzenie blokowe', readOnly: 'tylko odczyt', refresh: 'Odśwież', inPlaceDanger: 'Ta operacja zmienia istniejący USER. Przed jej rozpoczęciem zachowaj niezależny backup i sprawdź, czy wybrano właściwy nośnik.', restoreDanger: 'Przywrócenie nadpisze cały wybrany nośnik. Przed rozpoczęciem sprawdź urządzenie i zachowaj własny backup.', planReadOnly: 'Plan i kontrola nie zapisują danych na urządzeniu.', preparing: 'Przygotowywanie…', preparePlan: 'Przygotuj plan →', chooseFile: 'Wybierz plik', serverSaved: 'Zapisano na serwerze: {received} / {total}', browserSent: 'Przeglądarka wysłała: {sent} · plik {current} z {count} · zapisane części: {saved}', backupAdded: 'Backup dodany do sesji · {size}', fileAdded: 'Plik dodany do sesji · {size}', noFile: 'Nie wybrano pliku', planReady: 'Plan gotowy', existingUserExpansion: 'Rozszerzenie istniejącego USER in-place', target: 'Cel', sourceBackup: 'Backup', boot: 'BOOT', status: 'Stan', bootPairAdded: 'Dodano osobne pliki BOOT0 i BOOT1; zostaną sprawdzone podczas preflightu.', nextReadOnlyCheck: 'Następnie sprawdź pliki i urządzenie bez zapisu.', bootUnavailableTitle: 'BOOT0 / BOOT1 niedostępne przez adapter.', bootUnavailable: 'Możesz kontynuować preflight i pracę z RAWNAND/USER, ale backup BOOT0 oraz BOOT1 trzeba będzie przywrócić osobno z poziomu Hekate.', checking: 'Trwa kontrola…', checkFilesAndDevice: 'Sprawdź pliki i urządzenie', preflightComplete: 'Kontrola odczytowa zakończona', validFilesAndUser: 'Pliki i układ USER są poprawne', targetDevice: 'Nośnik docelowy:', adapterBootUnavailable: 'Adapter nie udostępnia BOOT0 i BOOT1. Po zakończeniu przywróć je osobno przez Hekate.', sourceFormat: 'Format źródła', sourceImage: 'Obraz źródłowy', backupParts: 'Części backupu', gptPartitions: 'Partycje GPT', user: 'USER', keysetUser: 'Keyset / USER', fat32User: 'FAT32 USER', expandedUser: 'USER po rozszerzeniu', fatGeometry: 'Nowa geometria FAT32', relocations: 'Relokacje', keysetValidated: 'Potwierdzony przez zaszyfrowany boot sector FAT32.', plainNoKeyValidation: 'Zwykłe odtworzenie nie wymaga walidacji klucza USER.', fatStats: '{allocated} zajętych · {free} wolnych · {bad} uszkodzonych klastrów · {chains} łańcuchów', fatGeometryValue: 'FAT: {current} → {target} sektorów · początek danych: +{shift} sektorów', relocationValue: '{count} zajętych klastrów zostanie odszyfrowanych i ponownie zaszyfrowanych.', inPlaceChecked: 'Sprawdzono układ partycji i dane USER bez zapisu na urządzeniu. Porównano obie kopie FAT i przygotowano plan przeniesienia danych.', restoreChecked: 'Sprawdzono układ backupu. Przy rozszerzaniu porównano obie kopie FAT i sprawdzono dane USER. Na urządzeniu docelowym niczego nie zapisano.', confirmTarget: 'Potwierdź cel, wpisując', startExpand: 'Rozpocznij rozszerzanie USER', startPlainRestore: 'Rozpocznij zwykłe odtwarzanie', startRestoreExpand: 'Rozpocznij przywracanie i rozszerzanie', noBootWritesTitle: 'Aplikacja nie zapisuje BOOT0 ani BOOT1.', noBootWrites: 'Odtwarzanie możesz uruchomić bez tej pary, a BOOT0 i BOOT1 przywrócić osobno przez Hekate.', task: 'ZADANIE', targetValue: 'Cel: {target}', taskNotStarted: 'Zadanie nie zostało jeszcze uruchomione.', recheckingTarget: 'Trwa ponowna kontrola celu i autoryzacja. Poczekaj na wynik.', waitingProgress: 'Oczekiwanie na pierwszy odczyt postępu…', from: '{processed} z {total}', writeVerified: 'Zapis i weryfikacja zakończone.', canceledSafely: 'Operacja anulowana na bezpiecznej granicy.', taskStopped: 'Zadanie zatrzymane', cancelRequested: 'Zażądano anulowania', cancelTask: 'Anuluj zadanie', cancellationSafety: 'Anulowanie następuje na bezpiecznej granicy przed końcowym commitem. Nie odłączaj nośnika, dopóki zadanie się nie zakończy.', doNotClose: 'Nie zamykaj okna podczas zapisu.', currentTask: 'BIEŻĄCE ZADANIE', running: 'W toku', completed: 'Zakończone', failed: 'Błąd', idle: 'Bezczynne', noActiveTask: 'Brak aktywnego zadania', openExecution: 'Otwórz krok Wykonanie, aby zobaczyć wynik.', targetCheck: 'Kontrola celu…', taskStatusHint: 'Po uruchomieniu operacji zobaczysz tutaj jej stan.', sessionLog: 'Dziennik sesji', logEntries: '{count} wpisów', emptyLog: 'Tu pojawią się zdarzenia z bieżącej sesji: kontrola, etapy zapisu i wynik.', genericRequestError: 'Nie udało się wykonać żądania.', uploadInterrupted: 'Przesyłanie pliku zostało przerwane.', uploadFailed: 'Nie udało się przesłać pliku.', serverStoppedUpload: 'Serwer przerwał upload; wybierz pliki ponownie.', readUploadProgressFailed: 'Nie udało się odczytać postępu uploadu: {error}', filePickerFailed: 'Nie udało się wybrać pliku: {error}', devicesFailed: 'Nie udało się pobrać urządzeń: {error}', progressFailed: 'Nie udało się odczytać postępu: {error}', chooseKeysetAndDevice: 'Wybierz keyset oraz urządzenie z istniejącą partycją USER.', chooseBackupKeysetDevice: 'Wybierz backup, keyset oraz docelowe urządzenie.', chooseBackupDevice: 'Wybierz backup oraz docelowe urządzenie.', bootPairRequired: 'Dodaj BOOT0 i BOOT1 jako kompletną parę albo pozostaw oba pola puste.', planPrepared: 'Plan przygotowany. Sprawdź cel i uruchom kontrolę odczytową.', planFailed: 'Nie udało się przygotować planu.', preflightStarted: 'Rozpoczęto kontrolę odczytową.', preflightPassed: 'Kontrola odczytowa zakończona pomyślnie.', preflightFailed: 'Kontrola odczytowa nie powiodła się.', preflightError: 'Kontrola nie powiodła się: {error}', typeFullTarget: 'Po udanym preflightcie wpisz dokładną, pełną ścieżkę urządzenia docelowego.', authorizationStarted: 'Trwa ponowna kontrola celu i autoryzacja zapisu.', taskStarted: 'Zadanie uruchomione.', startFailed: 'Nie udało się rozpocząć operacji: {error}', taskStartFailed: 'Nie udało się rozpocząć zadania.', cancelRequestedLog: 'Zażądano anulowania. Aplikacja zatrzyma zapis na bezpiecznej granicy.', cancelFailed: 'Nie udało się zażądać anulowania: {error}', phase_copying_payload: 'Kopiowanie obrazu', phase_verifying_payload: 'Weryfikacja obrazu', phase_committing_primary_metadata: 'Zapis primary GPT', phase_verifying_primary_metadata: 'Weryfikacja primary GPT', phase_relocating_user: 'Relokacja danych USER', phase_writing_fat_mirrors: 'Zapis obu kopii FAT', phase_committing_backup_gpt: 'Zapis backup GPT', phase_committing_primary_gpt: 'Zapis primary GPT', phase_committing_backup_boot_sector: 'Zapis zapasowego boot sectora', phase_committing_primary_boot_sector: 'Zapis głównego boot sectora', phase_verifying_expanded_user: 'Weryfikacja USER', operationFailed: 'Zadanie zakończyło się błędem. Sprawdź komunikat w kroku Wykonanie.', operationCanceled: 'Zadanie anulowano.', titlebar: { window: 'Okno NANDuNX', minimize: 'Minimalizuj okno', maximize: 'Maksymalizuj lub przywróć okno', close: 'Zamknij okno', waitForTask: 'Poczekaj na zakończenie zadania', windowControlFailed: 'Nie udało się sterować oknem: {error}' },
};

const translations = { en, pl };

pl.verifiedBytes = 'zweryfikowano {value}';

export function translate(locale, key, values = {}) {
  const message = key.split('.').reduce((value, part) => value?.[part], translations[locale] ?? en)
    ?? key.split('.').reduce((value, part) => value?.[part], en)
    ?? key;
  return String(message).replace(/\{(\w+)\}/g, (_, name) => values[name] ?? `{${name}}`);
}

export function initialLocale() {
  try {
    return locales[window.localStorage.getItem(LOCALE_STORAGE_KEY)] ? window.localStorage.getItem(LOCALE_STORAGE_KEY) : DEFAULT_LOCALE;
  } catch {
    return DEFAULT_LOCALE;
  }
}

// Legacy UI text is converted at the rendering boundary while the operation
// workflow is progressively moved to the keyed catalog above. Keeping this
// table separate makes the Polish source text available to translators and
// avoids coupling locale choice to NAND or device logic.
const legacyText = {
  'Przywracanie NAND': 'NAND restore', 'PRZEBIEG': 'WORKFLOW', 'Kroki operacji': 'Operation steps', 'Przygotowanie': 'Preparation', 'Plan i kontrola': 'Plan and checks', 'Wykonanie': 'Execution', 'Tryb, pliki i cel': 'Mode, files and target', 'Sprawdź przed zapisem': 'Review before writing', 'Postęp i wynik': 'Progress and result', 'OPERACJA NA NOŚNIKU': 'STORAGE OPERATION', 'Wybierz sposób pracy, własne pliki i urządzenie docelowe.': 'Choose an operation, your files, and the target device.', 'Sprawdź plan i wykonaj kontrolę odczytową przed zapisem.': 'Review the plan and run read-only checks before writing.', 'Tu zobaczysz postęp, wynik i możliwość anulowania.': 'View progress, results, and the option to cancel here.', 'Wybierz wariant': 'Choose an operation', 'Zwykłe przywrócenie': 'Plain restore', 'Odtwarza backup do docelowego nośnika bez zmiany rozmiaru USER.': 'Restores the backup to the target device without resizing USER.', 'Przywrócenie z rozszerzeniem USER': 'Restore and expand USER', 'USER zajmie dostępną przestrzeń na większym nośniku.': 'USER will use the available space on a larger device.', 'Rozszerz istniejący USER in-place': 'Expand existing USER in place', 'Powiększa USER na podłączonym nośniku. Wymaga osobnego backupu i poprawnego keysetu.': 'Expands USER on the connected device. Requires a separate backup and a valid keyset.', 'Dodaj pliki do bieżącej sesji': 'Add files to this session', 'Backup NAND': 'NAND backup', 'Wymagany do rozszerzenia USER; pozostaje tylko w tej sesji.': 'Required to expand USER; retained only for this session.', 'Opcjonalny przy zwykłym odtworzeniu; niezbędny później do operacji na USER.': 'Optional for plain restore; required later for USER operations.', 'Wybierz plik': 'Choose file', 'W wersji WWW upload jest przechowywany wyłącznie w prywatnym katalogu tymczasowym procesu i usuwany po jego zakończeniu.': 'In the Web edition, uploads are kept only in the process-private temporary directory and removed when it exits.', 'Wybierz nośnik docelowy': 'Choose the target device', 'Odczytywanie urządzeń…': 'Reading devices…', 'Wybierz urządzenie blokowe': 'Choose a block device', 'tylko odczyt': 'read-only', 'Odśwież': 'Refresh', 'Plan i kontrola nie zapisują danych na urządzeniu.': 'Planning and checks do not write data to the device.', 'Przygotowywanie…': 'Preparing…', 'Przygotuj plan →': 'Prepare plan →', 'Plan gotowy': 'Plan ready', 'Cel': 'Target', 'Stan': 'Status', 'Następnie sprawdź pliki i urządzenie bez zapisu.': 'Next, check the files and device without writing.', 'Trwa kontrola…': 'Checking…', 'Sprawdź pliki i urządzenie': 'Check files and device', 'Kontrola odczytowa zakończona': 'Read-only check complete', 'Pliki i układ USER są poprawne': 'Files and USER layout are valid', 'Nośnik docelowy:': 'Target device:', 'Format źródła': 'Source format', 'Obraz źródłowy': 'Source image', 'Części backupu': 'Backup parts', 'Partycje GPT': 'GPT partitions', 'Nowa geometria FAT32': 'New FAT32 geometry', 'Relokacje': 'Relocations', 'Potwierdź cel, wpisując': 'Confirm the target by typing', 'Rozpocznij rozszerzanie USER': 'Start USER expansion', 'Rozpocznij zwykłe odtwarzanie': 'Start plain restore', 'Rozpocznij przywracanie i rozszerzanie': 'Start restore and expansion', 'ZADANIE': 'TASK', 'Zadanie nie zostało jeszcze uruchomione.': 'The task has not started yet.', 'Zadanie zatrzymane': 'Task stopped', 'Anuluj zadanie': 'Cancel task', 'BIEŻĄCE ZADANIE': 'CURRENT TASK', 'W toku': 'Running', 'Zakończone': 'Completed', 'Błąd': 'Failed', 'Bezczynne': 'Idle', 'Brak aktywnego zadania': 'No active task', 'Dziennik sesji': 'Session log', 'wpisów': 'entries', 'Tu pojawią się zdarzenia z bieżącej sesji: kontrola, etapy zapisu i wynik.': 'Events from this session will appear here: checks, write phases, and the result.', 'Zamknij okno': 'Close window', 'Minimalizuj okno': 'Minimize window', 'Maksymalizuj lub przywróć okno': 'Maximize or restore window', 'Poczekaj na zakończenie zadania': 'Wait for the task to finish',
};

legacyText.Język = 'Language';

export function localizeLegacyContent(root, locale) {
  const pairs = locale === 'pl' ? Object.fromEntries(Object.entries(legacyText).map(([plText, enText]) => [enText, plText])) : legacyText;
  const replace = (value) => pairs[value] ?? value;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  nodes.forEach((node) => { const value = node.nodeValue; const trimmed = value.trim(); if (pairs[trimmed]) node.nodeValue = value.replace(trimmed, replace(trimmed)); });
  root.querySelectorAll('[aria-label],[title],[placeholder]').forEach((element) => ['aria-label', 'title', 'placeholder'].forEach((attribute) => { if (element.hasAttribute(attribute)) element.setAttribute(attribute, replace(element.getAttribute(attribute))); }));
}
