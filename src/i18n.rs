//! Interface messages and portable appearance preferences.
use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    English,
    Russian,
    French,
    German,
    Spanish,
    Italian,
}

impl Language {
    pub const ALL: [Self; 6] = [
        Self::English,
        Self::Russian,
        Self::French,
        Self::German,
        Self::Spanish,
        Self::Italian,
    ];

    pub fn name(self) -> &'static str {
        [
            "English",
            "Русский",
            "Français",
            "Deutsch",
            "Español",
            "Italiano",
        ][self as usize]
    }

    pub fn code(self) -> &'static str {
        ["en", "ru", "fr", "de", "es", "it"][self as usize]
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.code() == code)
    }

    /// Substitute numbered fields after lookup, without interpreting user data.
    pub fn format(self, key: &str, values: &[&dyn std::fmt::Display]) -> String {
        let template = self.text(key);
        let mut output = String::with_capacity(template.len());
        let mut remaining = template;
        while let Some(start) = remaining.find('{') {
            output.push_str(&remaining[..start]);
            let field = &remaining[start + 1..];
            if let Some(end) = field.find('}')
                && let Ok(index) = field[..end].parse::<usize>()
                && let Some(value) = values.get(index)
            {
                use std::fmt::Write;
                let _ = write!(output, "{value}");
                remaining = &field[end + 1..];
            } else {
                output.push('{');
                remaining = field;
            }
        }
        output.push_str(remaining);
        output
    }

    /// Localize known engine messages only. Paths, OS errors and release text
    /// remain verbatim; they are never scanned for translatable substrings.
    pub fn message(self, message: &str) -> Cow<'_, str> {
        let translated = self.text(message);
        if translated != message {
            return Cow::Borrowed(translated);
        }
        if let Some(rest) = message.strip_prefix("Filter clause ")
            && let Some((clause, reason)) = rest.split_once(": ")
        {
            return Cow::Owned(
                self.format("Filter clause {0}: {1}", &[&clause, &self.text(reason)]),
            );
        }
        for (prefix, template) in [
            ("Unable to resolve ", "Unable to resolve {0}"),
            ("Unable to inspect ", "Unable to inspect {0}"),
            ("Unable to read ", "Unable to read {0}"),
            (
                "Unable to start the scan: ",
                "Unable to start the scan: {0}",
            ),
            (
                "Unable to start live scanning: ",
                "Unable to start live scanning: {0}",
            ),
            (
                "Unable to start directory scanning: ",
                "Unable to start directory scanning: {0}",
            ),
            ("Could not reveal entry: ", "Could not reveal entry: {0}"),
            ("Incomplete scan preview: ", "Incomplete scan preview: {0}"),
            ("Could not save report: ", "Could not save report: {0}"),
            (
                "Could not start update worker: ",
                "Could not start update worker: {0}",
            ),
            (
                "Could not prepare the update restart: ",
                "Could not prepare the update restart: {0}",
            ),
            ("Partial live update: ", "Partial live update: {0}"),
            ("Saved ", "Saved {0}"),
        ] {
            if let Some(detail) = message.strip_prefix(prefix) {
                return Cow::Owned(self.format(template, &[&detail]));
            }
        }
        if let Some(rest) = message.strip_prefix("Watcher error (") {
            for (suffix, template) in [
                (
                    ") · polling every 2s",
                    "Watcher error ({0}) · polling every 2s",
                ),
                (
                    ") · resyncing, then polling",
                    "Watcher error ({0}) · resyncing, then polling",
                ),
            ] {
                if let Some(detail) = rest.strip_suffix(suffix) {
                    return Cow::Owned(self.format(template, &[&detail]));
                }
            }
        }
        if let Some(detail) = message.strip_suffix(" · waiting for recovery") {
            return Cow::Owned(self.format("{0} · waiting for recovery", &[&detail]));
        }
        for (suffix, template) in [
            (
                " is a link or reparse point. Select its target directly.",
                "{0} is a link or reparse point. Select its target directly.",
            ),
            (
                " is not a regular file or directory.",
                "{0} is not a regular file or directory.",
            ),
            (
                " is no longer a directory.",
                "{0} is no longer a directory.",
            ),
            (
                " became a link or reparse point during the scan.",
                "{0} became a link or reparse point during the scan.",
            ),
        ] {
            if let Some(path) = message.strip_suffix(suffix) {
                return Cow::Owned(self.format(template, &[&path]));
            }
        }
        Cow::Borrowed(message)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    #[default]
    Dark,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub error: Color32,
}

impl Theme {
    pub fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette {
                background: Color32::from_rgb(17, 22, 32),
                panel: Color32::from_rgb(23, 30, 42),
                muted: Color32::from_rgb(156, 171, 192),
                accent: Color32::from_rgb(100, 207, 187),
                on_accent: Color32::from_rgb(17, 22, 32),
                error: Color32::from_rgb(255, 151, 151),
            },
            Self::Light => Palette {
                background: Color32::from_rgb(244, 247, 250),
                panel: Color32::from_rgb(255, 255, 255),
                muted: Color32::from_rgb(77, 94, 116),
                accent: Color32::from_rgb(0, 105, 90),
                on_accent: Color32::WHITE,
                error: Color32::from_rgb(171, 37, 46),
            },
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        let palette = self.palette();
        ctx.set_theme(match self {
            Self::Light => egui::Theme::Light,
            Self::Dark => egui::Theme::Dark,
        });
        let mut visuals = match self {
            Self::Light => egui::Visuals::light(),
            Self::Dark => egui::Visuals::dark(),
        };
        visuals.panel_fill = palette.panel;
        visuals.window_fill = palette.panel;
        visuals.extreme_bg_color = palette.background;
        visuals.selection.bg_fill = match self {
            Self::Light => Color32::from_rgb(188, 226, 218),
            Self::Dark => Color32::from_rgb(41, 90, 90),
        };
        visuals.hyperlink_color = palette.accent;
        visuals.error_fg_color = palette.error;
        ctx.set_visuals(visuals);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Preferences {
    pub language: Language,
    pub theme: Theme,
}

impl Preferences {
    pub const STORAGE_KEY: &str = "interface-preferences";

    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|storage| eframe::get_value(storage, Self::STORAGE_KEY))
            .unwrap_or_default()
    }

    pub fn save(self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, Self::STORAGE_KEY, &self);
    }
}

macro_rules! messages {
    ($(($en:literal, $ru:literal, $fr:literal, $de:literal, $es:literal, $it:literal)),* $(,)?) => {
        #[cfg(test)]
        const MESSAGES: &[[&str; 6]] = &[$([$en, $ru, $fr, $de, $es, $it]),*];
        impl Language {
            pub fn text(self, key: &str) -> &str {
                match key {
                    $($en => [$en, $ru, $fr, $de, $es, $it][self as usize],)*
                    _ => key,
                }
            }
        }
    };
}

messages! {
    ("Language", "Язык", "Langue", "Sprache", "Idioma", "Lingua"),
    ("Theme", "Тема", "Thème", "Design", "Tema", "Tema"),
    ("Light", "Светлая", "Clair", "Hell", "Claro", "Chiaro"),
    ("Dark", "Тёмная", "Sombre", "Dunkel", "Oscuro", "Scuro"),
    ("Open folder…", "Открыть папку…", "Ouvrir un dossier…", "Ordner öffnen…", "Abrir carpeta…", "Apri cartella…"),
    ("ROOT PATH", "КОРНЕВОЙ ПУТЬ", "CHEMIN RACINE", "STAMMPFAD", "RUTA RAÍZ", "PERCORSO RADICE"),
    ("Any drive, folder or network path", "Диск, папка или сетевой путь", "Disque, dossier ou chemin réseau", "Laufwerk, Ordner oder Netzwerkpfad", "Unidad, carpeta o ruta de red", "Unità, cartella o percorso di rete"),
    ("Watch root", "Следить", "Surveiller", "Überwachen", "Supervisar", "Monitora"),
    ("Stop", "Остановить", "Arrêter", "Stoppen", "Detener", "Interrompi"),
    ("Starting…", "Запуск…", "Démarrage…", "Startet…", "Iniciando…", "Avvio…"),
    ("Scanning…", "Сканирование…", "Analyse…", "Wird eingelesen…", "Analizando…", "Scansione…"),
    ("LIVE", "В РЕАЛЬНОМ ВРЕМЕНИ", "EN DIRECT", "LIVE", "EN VIVO", "IN TEMPO REALE"),
    ("AUTO", "АВТО", "AUTO", "AUTO", "AUTO", "AUTO"),
    ("Stopped", "Остановлено", "Arrêté", "Gestoppt", "Detenido", "Interrotto"),
    ("Monitoring stopped", "Наблюдение остановлено", "Surveillance arrêtée", "Überwachung gestoppt", "Supervisión detenida", "Monitoraggio interrotto"),
    ("Choose a root", "Выберите путь", "Choisir une racine", "Stammpfad wählen", "Elige una raíz", "Scegli una radice"),
    ("Choose any drive or folder to watch", "Выберите диск или папку для наблюдения", "Choisir un disque ou dossier à surveiller", "Laufwerk oder Ordner zum Überwachen wählen", "Elige una unidad o carpeta para supervisar", "Scegli un'unità o cartella da monitorare"),
    ("Found {0} files · {1}", "Найдено файлов: {0} · {1}", "{0} fichiers trouvés · {1}", "{0} Dateien gefunden · {1}", "{0} archivos encontrados · {1}", "{0} file trovati · {1}"),
    ("Discovered so far", "Найдено на данный момент", "Découvert jusqu'ici", "Bisher gefunden", "Descubierto hasta ahora", "Rilevato finora"),
    ("{0} files · {1} folders", "Файлов: {0} · папок: {1}", "{0} fichiers · {1} dossiers", "{0} Dateien · {1} Ordner", "{0} archivos · {1} carpetas", "{0} file · {1} cartelle"),
    ("FILTER", "ФИЛЬТР", "FILTRE", "FILTER", "FILTRO", "FILTRO"),
    ("Clear filter", "Сбросить фильтр", "Effacer le filtre", "Filter löschen", "Borrar filtro", "Cancella filtro"),
    ("Depth", "Глубина", "Profondeur", "Tiefe", "Profundidad", "Profondità"),
    ("Refresh drives", "Обновить диски", "Actualiser les disques", "Laufwerke aktualisieren", "Actualizar unidades", "Aggiorna unità"),
    ("Color by volume", "Цвета по томам", "Couleurs par volume", "Farben nach Volume", "Colores por volumen", "Colori per volume"),
    ("Volume: {0}", "Том: {0}", "Volume : {0}", "Volume: {0}", "Volumen: {0}", "Volume: {0}"),
    ("Volumes in view", "Тома на карте", "Volumes affichés", "Sichtbare Volumes", "Volúmenes visibles", "Volumi visibili"),
    ("Mixed / grouped / unknown", "Разные / сгруппированы / неизвестно", "Mixtes / regroupés / inconnus", "Gemischt / gruppiert / unbekannt", "Mixtos / agrupados / desconocidos", "Misti / raggruppati / sconosciuti"),
    ("Volume information unavailable", "Информация о томе недоступна", "Informations du volume indisponibles", "Volume-Informationen nicht verfügbar", "Información del volumen no disponible", "Informazioni sul volume non disponibili"),
    ("Folder totals may include other volumes.", "Размер папки может включать другие тома.", "Le total du dossier peut inclure d’autres volumes.", "Ordnersummen können andere Volumes enthalten.", "El total de la carpeta puede incluir otros volúmenes.", "Il totale della cartella può includere altri volumi."),
    ("Classic colors", "Классические цвета", "Couleurs classiques", "Klassische Farben", "Colores clásicos", "Colori classici"),
    ("Discovered entries", "Найденные элементы", "Éléments découverts", "Gefundene Einträge", "Elementos descubiertos", "Elementi rilevati"),
    ("Largest entries", "Крупнейшие элементы", "Éléments les plus volumineux", "Größte Einträge", "Elementos más grandes", "Elementi più grandi"),
    ("Export CSV", "Экспорт CSV", "Exporter en CSV", "CSV exportieren", "Exportar CSV", "Esporta CSV"),
    ("Available after the scan completes", "Доступно после завершения сканирования", "Disponible après l'analyse", "Nach Abschluss des Einlesens verfügbar", "Disponible al terminar el análisis", "Disponibile al termine della scansione"),
    ("Help", "Справка", "Aide", "Hilfe", "Ayuda", "Guida"),
    ("SELECTED", "ВЫБРАНО", "SÉLECTION", "AUSWAHL", "SELECCIÓN", "SELEZIONE"),
    ("Matching {0}", "По фильтру: {0}", "Correspondance : {0}", "Passend: {0}", "Coincidente: {0}", "Corrispondente: {0}"),
    ("Copy path", "Копировать путь", "Copier le chemin", "Pfad kopieren", "Copiar ruta", "Copia percorso"),
    ("Reveal", "Показать", "Afficher", "Anzeigen", "Mostrar", "Mostra"),
    ("Zoom", "Приблизить", "Zoomer", "Vergrößern", "Ampliar", "Ingrandisci"),
    ("Finding matching entries…", "Поиск подходящих элементов…", "Recherche des correspondances…", "Passende Einträge werden gesucht…", "Buscando coincidencias…", "Ricerca delle corrispondenze…"),
    ("No entries", "Нет элементов", "Aucun élément", "Keine Einträge", "No hay elementos", "Nessun elemento"),
    ("Showing the first 100 discovered entries", "Показаны первые 100 найденных элементов", "Les 100 premiers éléments découverts", "Die ersten 100 gefundenen Einträge", "Se muestran los primeros 100 elementos", "Primi 100 elementi rilevati"),
    ("Showing 100 largest entries", "Показаны 100 крупнейших элементов", "Les 100 éléments les plus volumineux", "Die 100 größten Einträge", "Se muestran los 100 elementos más grandes", "I 100 elementi più grandi"),
    ("Scanning · {0}s · {1} files on map", "Сканирование · {0} с · файлов на карте: {1}", "Analyse · {0} s · {1} fichiers sur la carte", "Einlesen · {0} s · {1} Dateien auf der Karte", "Analizando · {0} s · {1} archivos en el mapa", "Scansione · {0} s · {1} file nella mappa"),
    ("Partial map · {0} discovered files", "Неполная карта · найдено файлов: {0}", "Carte partielle · {0} fichiers découverts", "Teilkarte · {0} Dateien gefunden", "Mapa parcial · {0} archivos descubiertos", "Mappa parziale · {0} file rilevati"),
    ("{0} updates · {1} skipped · {2} links · initial scan {3}s", "Изменений: {0} · пропущено: {1} · ссылок: {2} · сканирование: {3} с", "{0} mises à jour · {1} ignorés · {2} liens · analyse initiale {3} s", "{0} Änderungen · {1} übersprungen · {2} Links · erstes Einlesen {3} s", "{0} cambios · {1} omitidos · {2} enlaces · análisis inicial {3} s", "{0} aggiornamenti · {1} ignorati · {2} collegamenti · scansione iniziale {3} s"),
    ("Select a drive or folder to start watching.", "Выберите диск или папку для наблюдения.", "Choisissez un disque ou dossier à surveiller.", "Laufwerk oder Ordner zur Überwachung wählen.", "Elige una unidad o carpeta para supervisar.", "Scegli un'unità o cartella da monitorare."),
    ("Filtering · {0}%", "Фильтрация · {0}%", "Filtrage · {0} %", "Filtern · {0} %", "Filtrando · {0} %", "Filtro · {0}%"),
    ("Filtering…", "Фильтрация…", "Filtrage…", "Wird gefiltert…", "Filtrando…", "Filtro…"),
    ("Logical bytes", "Логический размер", "Octets logiques", "Logische Bytes", "Bytes lógicos", "Byte logici"),
    ("Up", "Выше", "Parent", "Übergeordnet", "Subir", "Su"),
    ("Scanning your files", "Сканирование файлов", "Analyse de vos fichiers", "Dateien werden eingelesen", "Analizando tus archivos", "Scansione dei file"),
    ("{0} files · {1} found", "Файлов: {0} · найдено: {1}", "{0} fichiers · {1} trouvés", "{0} Dateien · {1} gefunden", "{0} archivos · {1} encontrados", "{0} file · {1} trovati"),
    ("Choose a disk or folder", "Выберите диск или папку", "Choisissez un disque ou dossier", "Laufwerk oder Ordner wählen", "Elige un disco o carpeta", "Scegli un disco o cartella"),
    ("The map will follow file changes automatically.", "Карта будет обновляться при изменении файлов.", "La carte suivra automatiquement les changements.", "Die Karte folgt Dateiänderungen automatisch.", "El mapa reflejará los cambios automáticamente.", "La mappa seguirà automaticamente le modifiche."),
    ("Or select a drive in the sidebar.", "Или выберите диск на боковой панели.", "Ou sélectionnez un disque dans la barre latérale.", "Oder ein Laufwerk in der Seitenleiste wählen.", "O selecciona una unidad en la barra lateral.", "Oppure seleziona un'unità nella barra laterale."),
    ("{0} smaller entries", "Мелких элементов: {0}", "{0} petits éléments", "{0} kleinere Einträge", "{0} elementos pequeños", "{0} elementi piccoli"),
    ("Scanning…\nThe map fills as file sizes are discovered.", "Сканирование…\nКарта заполняется по мере обнаружения файлов.", "Analyse…\nLa carte se remplit au fil des découvertes.", "Einlesen…\nDie Karte füllt sich mit gefundenen Dateigrößen.", "Analizando…\nEl mapa se llena al descubrir los archivos.", "Scansione…\nLa mappa si riempie con i file rilevati."),
    ("No file bytes here yet.\nNew files will appear automatically.", "Здесь пока нет данных файлов.\nНовые файлы появятся автоматически.", "Aucune donnée ici pour l'instant.\nLes nouveaux fichiers apparaîtront automatiquement.", "Hier sind noch keine Dateidaten.\nNeue Dateien erscheinen automatisch.", "Aún no hay datos de archivos aquí.\nLos nuevos archivos aparecerán automáticamente.", "Nessun dato di file per ora.\nI nuovi file appariranno automaticamente."),
    ("No files match this filter.", "Нет файлов, подходящих под фильтр.", "Aucun fichier ne correspond à ce filtre.", "Keine Dateien entsprechen diesem Filter.", "Ningún archivo coincide con este filtro.", "Nessun file corrisponde al filtro."),
    ("Zoom into this folder or filter to inspect smaller files.", "Приблизьте папку или задайте фильтр для просмотра мелких файлов.", "Zoomez dans ce dossier ou filtrez pour voir les petits fichiers.", "Ordner vergrößern oder filtern, um kleinere Dateien zu sehen.", "Amplía esta carpeta o filtra para ver archivos pequeños.", "Ingrandisci la cartella o filtra per vedere i file piccoli."),
    ("Double-click to zoom in", "Двойной щелчок — приблизить", "Double-cliquez pour zoomer", "Doppelklicken zum Vergrößern", "Doble clic para ampliar", "Doppio clic per ingrandire"),
    ("Copy selected path", "Копировать выбранный путь", "Copier le chemin sélectionné", "Ausgewählten Pfad kopieren", "Copiar ruta seleccionada", "Copia percorso selezionato"),
    ("Zoom into selection", "Приблизить выбранное", "Zoomer sur la sélection", "Auswahl vergrößern", "Ampliar selección", "Ingrandisci selezione"),
    ("Back to root", "К корню", "Retour à la racine", "Zurück zum Stamm", "Volver a la raíz", "Torna alla radice"),
    ("Dismiss", "Скрыть", "Fermer", "Schließen", "Cerrar", "Chiudi"),
    ("Error", "Ошибка", "Erreur", "Fehler", "Error", "Errore"),
    ("Unable to resolve {0}", "Не удалось определить путь {0}", "Impossible de résoudre {0}", "Pfad konnte nicht aufgelöst werden: {0}", "No se pudo resolver {0}", "Impossibile risolvere {0}"),
    ("Unable to inspect {0}", "Не удалось прочитать сведения о {0}", "Impossible d'inspecter {0}", "Eintrag konnte nicht geprüft werden: {0}", "No se pudo inspeccionar {0}", "Impossibile esaminare {0}"),
    ("Unable to read {0}", "Не удалось прочитать {0}", "Impossible de lire {0}", "Eintrag konnte nicht gelesen werden: {0}", "No se pudo leer {0}", "Impossibile leggere {0}"),
    ("Unable to start the scan: {0}", "Не удалось начать сканирование: {0}", "Impossible de démarrer l'analyse : {0}", "Einlesen konnte nicht gestartet werden: {0}", "No se pudo iniciar el análisis: {0}", "Impossibile avviare la scansione: {0}"),
    ("Unable to start live scanning: {0}", "Не удалось начать сканирование с наблюдением: {0}", "Impossible de démarrer l'analyse en direct : {0}", "Live-Einlesen konnte nicht gestartet werden: {0}", "No se pudo iniciar el análisis en vivo: {0}", "Impossibile avviare la scansione in tempo reale: {0}"),
    ("Unable to start directory scanning: {0}", "Не удалось запустить сканирование папок: {0}", "Impossible de démarrer l'analyse des dossiers : {0}", "Ordnereinlesen konnte nicht gestartet werden: {0}", "No se pudo iniciar el análisis de carpetas: {0}", "Impossibile avviare la scansione delle cartelle: {0}"),
    ("Could not reveal entry: {0}", "Не удалось показать элемент: {0}", "Impossible d'afficher l'élément : {0}", "Eintrag konnte nicht angezeigt werden: {0}", "No se pudo mostrar el elemento: {0}", "Impossibile mostrare l'elemento: {0}"),
    ("{0} is a link or reparse point. Select its target directly.", "{0} — ссылка или точка повторного анализа. Выберите её целевой путь.", "{0} est un lien ou point de réanalyse. Sélectionnez directement sa cible.", "{0} ist ein Link oder Analysepunkt. Ziel direkt auswählen.", "{0} es un enlace o punto de reanálisis. Selecciona su destino directamente.", "{0} è un collegamento o punto di analisi. Seleziona direttamente la destinazione."),
    ("{0} is not a regular file or directory.", "{0} не является обычным файлом или папкой.", "{0} n'est pas un fichier ou dossier ordinaire.", "{0} ist keine reguläre Datei oder kein Ordner.", "{0} no es un archivo o directorio normal.", "{0} non è un file o cartella normale."),
    ("{0} is no longer a directory.", "{0} больше не является папкой.", "{0} n'est plus un dossier.", "{0} ist kein Ordner mehr.", "{0} ya no es un directorio.", "{0} non è più una cartella."),
    ("{0} became a link or reparse point during the scan.", "{0} стал ссылкой или точкой повторного анализа во время сканирования.", "{0} est devenu un lien ou point de réanalyse pendant l'analyse.", "{0} wurde beim Einlesen zu einem Link oder Analysepunkt.", "{0} se convirtió en enlace o punto de reanálisis durante el análisis.", "{0} è diventato un collegamento o punto di analisi durante la scansione."),
    ("Directory scan workers stopped unexpectedly", "Обработчики сканирования папок неожиданно остановились", "Les tâches d'analyse des dossiers se sont arrêtées inopinément", "Die Ordnereinlesung wurde unerwartet beendet", "El análisis de carpetas se detuvo inesperadamente", "La scansione delle cartelle si è interrotta in modo imprevisto"),
    ("Scan cancelled", "Сканирование отменено", "Analyse annulée", "Einlesen abgebrochen", "Análisis cancelado", "Scansione annullata"),
    ("Export disk usage report", "Экспорт отчёта о диске", "Exporter le rapport d'utilisation du disque", "Bericht zur Datenträgerbelegung exportieren", "Exportar informe de uso del disco", "Esporta rapporto sull'uso del disco"),
    ("CSV report", "Отчёт CSV", "Rapport CSV", "CSV-Bericht", "Informe CSV", "Rapporto CSV"),
    ("Saved {0}", "Сохранено: {0}", "Enregistré : {0}", "Gespeichert: {0}", "Guardado: {0}", "Salvato: {0}"),
    ("Could not save report: {0}", "Не удалось сохранить отчёт: {0}", "Impossible d'enregistrer le rapport : {0}", "Bericht konnte nicht gespeichert werden: {0}", "No se pudo guardar el informe: {0}", "Impossibile salvare il rapporto: {0}"),
    ("Incomplete scan preview: {0}", "Неполный предварительный результат: {0}", "Aperçu d'analyse incomplet : {0}", "Unvollständige Vorschau: {0}", "Vista previa incompleta: {0}", "Anteprima di scansione incompleta: {0}"),
    ("Preparing the completed map stopped unexpectedly", "Подготовка готовой карты неожиданно остановлена", "La préparation de la carte s'est arrêtée inopinément", "Die Kartenvorbereitung wurde unerwartet beendet", "La preparación del mapa se detuvo inesperadamente", "La preparazione della mappa si è interrotta in modo imprevisto"),
    ("Choose any drive or folder. Changes update the map automatically.", "Выберите диск или папку. Изменения обновляют карту автоматически.", "Choisissez un disque ou dossier. La carte se met à jour automatiquement.", "Laufwerk oder Ordner wählen. Änderungen aktualisieren die Karte automatisch.", "Elige una unidad o carpeta. Los cambios actualizan el mapa automáticamente.", "Scegli un'unità o cartella. Le modifiche aggiornano la mappa automaticamente."),
    ("The first scan fills the map progressively; sizes show discovered data.", "Первое сканирование постепенно заполняет карту; размеры отражают найденные данные.", "L'analyse initiale remplit la carte progressivement ; les tailles reflètent les données découvertes.", "Das erste Einlesen füllt die Karte schrittweise; Größen zeigen gefundene Daten.", "El primer análisis llena el mapa progresivamente; los tamaños reflejan los datos descubiertos.", "La prima scansione riempie la mappa gradualmente; le dimensioni indicano i dati rilevati."),
    ("Click: inspect. Double-click: zoom. Backspace: parent.", "Щелчок: сведения. Двойной щелчок: приблизить. Backspace: выше.", "Clic : inspecter. Double-clic : zoomer. Retour arrière : parent.", "Klick: prüfen. Doppelklick: vergrößern. Rücktaste: übergeordnet.", "Clic: inspeccionar. Doble clic: ampliar. Retroceso: subir.", "Clic: dettagli. Doppio clic: ingrandisci. Backspace: cartella superiore."),
    ("{0}: choose root. F5: optional full rescan.", "{0}: выбрать путь. F5: повторное полное сканирование.", "{0} : choisir la racine. F5 : nouvelle analyse complète.", "{0}: Stamm wählen. F5: erneut vollständig einlesen.", "{0}: elegir raíz. F5: repetir el análisis completo.", "{0}: scegli radice. F5: ripeti la scansione completa."),
    ("Combine filename and size filters with semicolons.", "Разделяйте фильтры имени и размера точкой с запятой.", "Combinez les filtres de nom et taille avec des points-virgules.", "Namens- und Größenfilter mit Semikolon kombinieren.", "Combina filtros de nombre y tamaño con punto y coma.", "Combina filtri per nome e dimensione con punti e virgola."),
    ("MB uses decimal bytes; MiB uses binary bytes.", "MB — десятичные байты; MiB — двоичные байты.", "MB utilise des octets décimaux ; MiB des octets binaires.", "MB verwendet dezimale Bytes, MiB binäre Bytes.", "MB usa bytes decimales; MiB usa bytes binarios.", "MB usa byte decimali; MiB usa byte binari."),
    ("Logical file sizes; hard links count per path.", "Логические размеры; жёсткие ссылки учитываются по каждому пути.", "Tailles logiques ; les liens physiques comptent par chemin.", "Logische Dateigrößen; harte Links zählen je Pfad.", "Tamaños lógicos; los enlaces duros cuentan por ruta.", "Dimensioni logiche; i collegamenti fisici contano per percorso."),
    ("Links and reparse points are not traversed.", "Ссылки и точки повторного анализа не обходятся.", "Les liens et points de réanalyse ne sont pas parcourus.", "Links und Analysepunkte werden nicht durchlaufen.", "No se recorren enlaces ni puntos de reanálisis.", "Collegamenti e punti di analisi non vengono attraversati."),
    ("Watcher errors trigger automatic recovery.", "Ошибки наблюдения запускают автоматическое восстановление.", "Les erreurs de surveillance déclenchent une récupération automatique.", "Überwachungsfehler lösen automatische Wiederherstellung aus.", "Los errores de supervisión activan la recuperación automática.", "Gli errori di monitoraggio avviano il ripristino automatico."),
    ("MIT licensed · No telemetry · No deletion actions", "Лицензия MIT · Без телеметрии · Без удаления файлов", "Licence MIT · Sans télémétrie · Sans suppression", "MIT-Lizenz · Keine Telemetrie · Keine Löschaktionen", "Licencia MIT · Sin telemetría · Sin eliminación", "Licenza MIT · Nessuna telemetria · Nessuna eliminazione"),
    ("Live updates active · native filesystem watcher", "Изменения отслеживаются · системное наблюдение", "Mises à jour en direct · surveillance native", "Live-Änderungen aktiv · native Dateisystemüberwachung", "Cambios en vivo activos · supervisor nativo", "Aggiornamenti attivi · monitoraggio nativo"),
    ("Native watching unavailable · polling every 2s; retrying native watcher", "Системное наблюдение недоступно · проверка каждые 2 с; повторное подключение", "Surveillance native indisponible · vérification toutes les 2 s ; nouvelle tentative", "Native Überwachung nicht verfügbar · Prüfung alle 2 s; erneuter Versuch", "Supervisor nativo no disponible · comprobación cada 2 s; reintentando", "Monitoraggio nativo non disponibile · controllo ogni 2 s; nuovo tentativo"),
    ("No watchable ancestor · polling every 2s", "Нет доступного родительского пути · проверка каждые 2 с", "Aucun parent surveillable · vérification toutes les 2 s", "Kein überwachbarer Elternpfad · Prüfung alle 2 s", "Sin ruta superior supervisable · comprobación cada 2 s", "Nessun percorso superiore monitorabile · controllo ogni 2 s"),
    ("Scanning · the map fills as files are found", "Сканирование · карта заполняется найденными файлами", "Analyse · la carte se remplit avec les fichiers trouvés", "Einlesen · die Karte füllt sich mit gefundenen Dateien", "Analizando · el mapa se llena con los archivos encontrados", "Scansione · la mappa si riempie con i file rilevati"),
    ("Filesystem events were lost or uncertain · resyncing", "События файловой системы потеряны · синхронизация", "Événements perdus ou incertains · resynchronisation", "Dateisystemereignisse verloren oder unklar · Abgleich", "Eventos perdidos o inciertos · resincronizando", "Eventi persi o incerti · risincronizzazione"),
    ("Watcher error ({0}) · polling every 2s", "Ошибка наблюдения ({0}) · проверка каждые 2 с", "Erreur de surveillance ({0}) · vérification toutes les 2 s", "Überwachungsfehler ({0}) · Prüfung alle 2 s", "Error de supervisión ({0}) · comprobación cada 2 s", "Errore di monitoraggio ({0}) · controllo ogni 2 s"),
    ("Watcher error ({0}) · resyncing, then polling", "Ошибка наблюдения ({0}) · синхронизация, затем периодическая проверка", "Erreur de surveillance ({0}) · resynchronisation, puis vérifications", "Überwachungsfehler ({0}) · Abgleich, dann regelmäßige Prüfung", "Error de supervisión ({0}) · resincronizando y luego comprobando", "Errore di monitoraggio ({0}) · risincronizzazione, poi controlli periodici"),
    ("{0} · waiting for recovery", "{0} · ожидание восстановления", "{0} · en attente de récupération", "{0} · Wiederherstellung wird erwartet", "{0} · esperando recuperación", "{0} · in attesa del ripristino"),
    ("Partial live update: {0}", "Неполное обновление: {0}", "Mise à jour partielle : {0}", "Unvollständige Live-Änderung: {0}", "Actualización parcial: {0}", "Aggiornamento parziale: {0}"),
    ("Filter clause {0}: {1}", "Условие фильтра {0}: {1}", "Clause du filtre {0} : {1}", "Filterbedingung {0}: {1}", "Condición del filtro {0}: {1}", "Condizione filtro {0}: {1}"),
    ("expected a pattern or size; empty clauses are not allowed.", "нужен шаблон или размер; пустые условия недопустимы.", "un motif ou une taille est requis ; les clauses vides sont interdites.", "Muster oder Größe erwartet; leere Bedingungen sind nicht zulässig.", "se esperaba un patrón o tamaño; no se permiten condiciones vacías.", "è richiesto un modello o una dimensione; le condizioni vuote non sono ammesse."),
    ("an exclusion needs a filename pattern after '|'.", "после '|' нужен шаблон исключаемых имён.", "un motif de nom est requis après '|'.", "Ein Ausschluss benötigt ein Namensmuster nach '|'.", "una exclusión necesita un patrón de nombre después de '|'.", "un'esclusione richiede un modello di nome dopo '|'."),
    ("expected a nonnegative size, for example >10MB or <1.5GiB.", "нужен неотрицательный размер, например >10MB или <1.5GiB.", "une taille positive ou nulle est requise, par exemple >10MB ou <1.5GiB.", "Nichtnegative Größe erwartet, etwa >10MB oder <1.5GiB.", "se esperaba un tamaño no negativo, por ejemplo >10MB o <1.5GiB.", "è richiesta una dimensione non negativa, ad esempio >10MB o <1.5GiB."),
    ("unknown size unit; use B, KB, MB, GB, TB, PB, EB, or IEC units such as MiB.", "неизвестная единица; используйте B, KB, MB, GB, TB, PB, EB или единицы IEC, например MiB.", "unité inconnue ; utilisez B, KB, MB, GB, TB, PB, EB ou une unité IEC comme MiB.", "Unbekannte Größeneinheit; B, KB, MB, GB, TB, PB, EB oder IEC-Einheiten wie MiB verwenden.", "unidad desconocida; usa B, KB, MB, GB, TB, PB, EB o unidades IEC como MiB.", "unità sconosciuta; usa B, KB, MB, GB, TB, PB, EB o unità IEC come MiB."),
    ("a size can contain only one decimal point.", "размер может содержать только одну десятичную точку.", "une taille ne peut contenir qu'un point décimal.", "Eine Größe darf nur einen Dezimalpunkt enthalten.", "un tamaño solo puede contener un punto decimal.", "una dimensione può contenere un solo punto decimale."),
    ("size is too large; the maximum is 18446744073709551615 bytes.", "слишком большой размер; максимум — 18446744073709551615 байт.", "taille trop grande ; le maximum est de 18446744073709551615 octets.", "Größe zu groß; maximal 18446744073709551615 Bytes.", "tamaño demasiado grande; el máximo es 18446744073709551615 bytes.", "dimensione troppo grande; il massimo è 18446744073709551615 byte."),
    ("size has too many decimal places.", "слишком много знаков после десятичной точки.", "la taille a trop de décimales.", "Die Größe hat zu viele Dezimalstellen.", "el tamaño tiene demasiados decimales.", "la dimensione ha troppe cifre decimali."),
    ("size must resolve to a whole number of bytes.", "размер должен соответствовать целому числу байт.", "la taille doit correspondre à un nombre entier d'octets.", "Die Größe muss einer ganzen Byteanzahl entsprechen.", "el tamaño debe equivaler a un número entero de bytes.", "la dimensione deve corrispondere a un numero intero di byte."),
    ("Update available", "Доступно обновление", "Mise à jour disponible", "Update verfügbar", "Actualización disponible", "Aggiornamento disponibile"),
    ("Updates", "Обновления", "Mises à jour", "Updates", "Actualizaciones", "Aggiornamenti"),
    ("App updates", "Обновления приложения", "Mises à jour de l'application", "Anwendungsupdates", "Actualizaciones de la aplicación", "Aggiornamenti dell'app"),
    ("Check the latest stable release on GitHub. Download and installation require your action.", "Проверка стабильной версии на GitHub. Загрузка и установка выполняются по вашему действию.", "Vérifiez la dernière version stable sur GitHub. Le téléchargement et l'installation nécessitent votre action.", "Neueste stabile Version auf GitHub prüfen. Download und Installation erfordern Ihre Aktion.", "Comprueba la última versión estable en GitHub. La descarga e instalación requieren tu acción.", "Controlla l'ultima versione stabile su GitHub. Download e installazione richiedono una tua azione."),
    ("Check for updates on startup", "Проверять обновления при запуске", "Vérifier les mises à jour au démarrage", "Beim Start nach Updates suchen", "Buscar actualizaciones al iniciar", "Cerca aggiornamenti all'avvio"),
    ("Check GitHub for a newer version.", "Проверьте новую версию на GitHub.", "Recherchez une nouvelle version sur GitHub.", "Auf GitHub nach einer neueren Version suchen.", "Busca una versión más reciente en GitHub.", "Cerca una versione più recente su GitHub."),
    ("Checking GitHub…", "Проверка GitHub…", "Vérification de GitHub…", "GitHub wird geprüft…", "Comprobando GitHub…", "Controllo di GitHub…"),
    ("You are using the latest version.", "У вас установлена последняя версия.", "Vous utilisez la dernière version.", "Sie verwenden die neueste Version.", "Estás usando la última versión.", "Stai usando l'ultima versione."),
    ("A newer version is available.", "Доступна новая версия.", "Une nouvelle version est disponible.", "Eine neuere Version ist verfügbar.", "Hay una versión más reciente.", "È disponibile una versione più recente."),
    ("Cancel download", "Отменить загрузку", "Annuler le téléchargement", "Download abbrechen", "Cancelar descarga", "Annulla download"),
    ("Download verified. Ready to install.", "Загрузка проверена. Можно установить.", "Téléchargement vérifié. Prêt à installer.", "Download geprüft. Bereit zur Installation.", "Descarga verificada. Lista para instalar.", "Download verificato. Pronto per l'installazione."),
    ("Installing update…", "Установка обновления…", "Installation de la mise à jour…", "Update wird installiert…", "Instalando actualización…", "Installazione dell'aggiornamento…"),
    ("Restarting to finish the update…", "Перезапуск для завершения обновления…", "Redémarrage pour terminer la mise à jour…", "Neustart zum Abschluss des Updates…", "Reiniciando para completar la actualización…", "Riavvio per completare l'aggiornamento…"),
    ("Version {0}", "Версия {0}", "Version {0}", "Version {0}", "Versión {0}", "Versione {0}"),
    ("View release on GitHub", "Открыть выпуск на GitHub", "Voir la version sur GitHub", "Release auf GitHub ansehen", "Ver versión en GitHub", "Vedi versione su GitHub"),
    ("In-app installation supports Windows and Ubuntu x86-64. Use the release page for other platforms.", "Установка из приложения поддерживает Windows и Ubuntu x86-64. Для других платформ используйте страницу выпуска.", "L'installation intégrée prend en charge Windows et Ubuntu x86-64. Pour les autres plateformes, consultez la page de version.", "Die Installation in der App unterstützt Windows und Ubuntu x86-64. Für andere Plattformen die Release-Seite verwenden.", "La instalación integrada admite Windows y Ubuntu x86-64. Para otras plataformas, consulta la página de la versión.", "L'installazione integrata supporta Windows e Ubuntu x86-64. Per altre piattaforme, usa la pagina della versione."),
    ("This release has no verified update package. Use the release page to update manually.", "В выпуске нет проверенного пакета обновления. Обновите вручную со страницы выпуска.", "Cette version n'a pas de paquet de mise à jour vérifié. Utilisez la page de version pour la mise à jour manuelle.", "Dieses Release enthält kein geprüftes Update-Paket. Über die Release-Seite manuell aktualisieren.", "Esta versión no tiene un paquete de actualización verificado. Actualiza manualmente desde su página.", "Questa versione non ha un pacchetto di aggiornamento verificato. Aggiorna manualmente dalla pagina della versione."),
    ("Download and install", "Скачать и установить", "Télécharger et installer", "Herunterladen und installieren", "Descargar e instalar", "Scarica e installa"),
    ("Ubuntu will ask for permission to install the verified package. The app will restart after installation.", "Ubuntu запросит разрешение на установку проверенного пакета. После установки приложение перезапустится.", "Ubuntu demandera l'autorisation d'installer le paquet vérifié. L'application redémarrera après l'installation.", "Ubuntu bittet um Erlaubnis, das geprüfte Paket zu installieren. Danach startet die App neu.", "Ubuntu pedirá permiso para instalar el paquete verificado. La aplicación se reiniciará al terminar.", "Ubuntu chiederà il permesso di installare il pacchetto verificato. L'app si riavvierà al termine."),
    ("The app will replace its executable and restart. The current root will be scanned again.", "Приложение заменит свой EXE и перезапустится. Текущий путь будет просканирован заново.", "L'application remplacera son exécutable et redémarrera. La racine actuelle sera analysée à nouveau.", "Die App ersetzt ihre Programmdatei und startet neu. Der aktuelle Stamm wird erneut eingelesen.", "La aplicación reemplazará su ejecutable y se reiniciará. Se analizará de nuevo la raíz actual.", "L'app sostituirà l'eseguibile e si riavvierà. La radice attuale verrà analizzata di nuovo."),
    ("Install and restart", "Установить и перезапустить", "Installer et redémarrer", "Installieren und neu starten", "Instalar y reiniciar", "Installa e riavvia"),
    ("Release notes", "Что нового", "Notes de version", "Versionshinweise", "Notas de la versión", "Note di versione"),
    ("Check for updates", "Проверить обновления", "Vérifier les mises à jour", "Nach Updates suchen", "Buscar actualizaciones", "Cerca aggiornamenti"),
    ("GitHub Releases", "Выпуски GitHub", "Versions GitHub", "GitHub-Releases", "Versiones de GitHub", "Versioni GitHub"),
    ("The update worker stopped unexpectedly. Please try again.", "Проверка обновлений неожиданно остановилась. Повторите попытку.", "La tâche de mise à jour s'est arrêtée inopinément. Réessayez.", "Der Update-Vorgang wurde unerwartet beendet. Erneut versuchen.", "El proceso de actualización se detuvo inesperadamente. Inténtalo de nuevo.", "Il processo di aggiornamento si è interrotto in modo imprevisto. Riprova."),
    ("Could not start update worker: {0}", "Не удалось запустить обновление: {0}", "Impossible de démarrer la mise à jour : {0}", "Update-Vorgang konnte nicht gestartet werden: {0}", "No se pudo iniciar la actualización: {0}", "Impossibile avviare l'aggiornamento: {0}"),
    ("Could not prepare the update restart: {0}", "Не удалось подготовить перезапуск: {0}", "Impossible de préparer le redémarrage : {0}", "Update-Neustart konnte nicht vorbereitet werden: {0}", "No se pudo preparar el reinicio: {0}", "Impossibile preparare il riavvio: {0}"),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    fn fields(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(field, _)| field))
            .collect()
    }

    #[test]
    fn every_catalog_entry_is_complete_and_preserves_format_fields() {
        let mut keys = BTreeSet::new();
        for row in MESSAGES {
            assert!(keys.insert(row[0]), "duplicate catalog key {}", row[0]);
            for language in Language::ALL {
                let translated = row[language as usize];
                assert!(
                    !translated.trim().is_empty(),
                    "empty {}: {}",
                    language.code(),
                    row[0]
                );
                assert_eq!(
                    fields(row[0]),
                    fields(translated),
                    "{}: {}",
                    language.code(),
                    row[0]
                );
                assert_eq!(language.text(row[0]), translated);
            }
        }
    }

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);
    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.into(), value);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn existing_settings_gain_defaults_and_new_choices_survive_restart() {
        let mut storage = MemoryStorage::default();
        eframe::set_value(&mut storage, "check-for-updates-on-startup", &false);
        assert_eq!(Preferences::load(Some(&storage)), Preferences::default());
        let preferences = Preferences {
            language: Language::Italian,
            theme: Theme::Light,
        };
        preferences.save(&mut storage);
        assert_eq!(Preferences::load(Some(&storage)), preferences);
        assert_eq!(
            eframe::get_value::<bool>(&storage, "check-for-updates-on-startup"),
            Some(false)
        );
    }

    #[test]
    fn formatting_never_substitutes_fields_inside_user_paths() {
        let path = "C:/reports/{1}/Watch root.txt";
        let message = Language::French.format("Saved {0}", &[&path]);
        assert_eq!(message, "Enregistré : C:/reports/{1}/Watch root.txt");
        assert_eq!(Language::German.message(path), path);
        assert_eq!(
            Language::Spanish.message("Filter clause 2: size has too many decimal places."),
            "Condición del filtro 2: el tamaño tiene demasiados decimales."
        );
    }

    #[test]
    fn theme_changes_use_the_native_visuals_and_readable_palette() {
        let context = egui::Context::default();
        for theme in [Theme::Light, Theme::Dark] {
            theme.apply(&context);
            assert_eq!(context.style().visuals.dark_mode, theme == Theme::Dark);
            assert_eq!(
                context.theme(),
                if theme == Theme::Dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                }
            );
            assert_eq!(context.style().visuals.panel_fill, theme.palette().panel);
            assert_ne!(theme.palette().background, theme.palette().muted);
        }
    }
}
