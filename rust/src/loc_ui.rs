//! Interface strings for the screens rebuilt on the interface kit.
//!
//! `loc.rs` is written in capitals because the old 5x7 font had nothing else:
//! "RÉGLAGES", "PLUS JOUÉS", and in Turkish "SERİT" for "ŞERİT" because there was
//! no Ş to draw. The kit draws the console's own fonts, so these are written the
//! way each language writes them, accents and lower case included.
//!
//! Kept apart from `loc::Strings` while screens move over one at a time: a
//! screen on the kit reads here, a screen not yet moved reads `loc`. Same nine
//! languages, same order, same rule as `loc.rs`: a field added here is added
//! to every table, or nothing compiles.

use crate::loc::{current, Lang};

pub struct Ui {
    pub tab_play: &'static str,
    pub tab_import: &'static str,
    pub tab_settings: &'static str,
    pub hint_play: &'static str,
    pub hint_options: &'static str,
    pub hint_sort: &'static str,
    pub hint_search: &'static str,
    /// Beside the ZL / ZR glyphs on the home, when the library has folders.
    pub folders: &'static str,
    pub view_grid: &'static str,
    pub view_list: &'static str,
    pub view_strip: &'static str,
    pub view_shelf: &'static str,
}

const EN: Ui = Ui {
    tab_play: "Play",
    tab_import: "Import",
    tab_settings: "Settings",
    hint_play: "Play",
    hint_options: "Options",
    hint_sort: "Sort",
    hint_search: "Search",
    folders: "Folders",
    view_grid: "Grid",
    view_list: "List",
    view_strip: "Strip",
    view_shelf: "Shelf",
};

const FR: Ui = Ui {
    tab_play: "Jouer",
    tab_import: "Importer",
    tab_settings: "Réglages",
    hint_play: "Jouer",
    hint_options: "Options",
    hint_sort: "Trier",
    hint_search: "Rechercher",
    folders: "Dossiers",
    view_grid: "Grille",
    view_list: "Liste",
    view_strip: "Bande",
    view_shelf: "Étagère",
};

const ES: Ui = Ui {
    tab_play: "Jugar",
    tab_import: "Importar",
    tab_settings: "Ajustes",
    hint_play: "Jugar",
    hint_options: "Opciones",
    hint_sort: "Ordenar",
    hint_search: "Buscar",
    folders: "Carpetas",
    view_grid: "Cuadrícula",
    view_list: "Lista",
    view_strip: "Tira",
    view_shelf: "Estante",
};

const RU: Ui = Ui {
    tab_play: "Играть",
    tab_import: "Загрузка",
    tab_settings: "Настройки",
    hint_play: "Играть",
    hint_options: "Параметры",
    hint_sort: "Сортировка",
    hint_search: "Поиск",
    folders: "Папки",
    view_grid: "Сетка",
    view_list: "Список",
    view_strip: "Лента",
    view_shelf: "Полка",
};

const DE: Ui = Ui {
    tab_play: "Spielen",
    tab_import: "Import",
    tab_settings: "Einstellungen",
    hint_play: "Spielen",
    hint_options: "Optionen",
    hint_sort: "Sortieren",
    hint_search: "Suchen",
    folders: "Ordner",
    view_grid: "Raster",
    view_list: "Liste",
    view_strip: "Leiste",
    view_shelf: "Regal",
};

const IT: Ui = Ui {
    tab_play: "Gioca",
    tab_import: "Importa",
    tab_settings: "Impostazioni",
    hint_play: "Gioca",
    hint_options: "Opzioni",
    hint_sort: "Ordina",
    hint_search: "Cerca",
    folders: "Cartelle",
    view_grid: "Griglia",
    view_list: "Elenco",
    view_strip: "Striscia",
    view_shelf: "Scaffale",
};

const PT: Ui = Ui {
    tab_play: "Jogar",
    tab_import: "Importar",
    tab_settings: "Ajustes",
    hint_play: "Jogar",
    hint_options: "Opções",
    hint_sort: "Ordenar",
    hint_search: "Pesquisar",
    folders: "Pastas",
    view_grid: "Grade",
    view_list: "Lista",
    view_strip: "Faixa",
    view_shelf: "Prateleira",
};

const ZH: Ui = Ui {
    tab_play: "游戏",
    tab_import: "导入",
    tab_settings: "设置",
    hint_play: "开始",
    hint_options: "选项",
    hint_sort: "排序",
    hint_search: "搜索",
    folders: "文件夹",
    view_grid: "网格",
    view_list: "列表",
    view_strip: "横条",
    view_shelf: "书架",
};

const TR: Ui = Ui {
    tab_play: "Oyna",
    tab_import: "Aktar",
    tab_settings: "Ayarlar",
    hint_play: "Oyna",
    hint_options: "Seçenekler",
    hint_sort: "Sırala",
    hint_search: "Ara",
    folders: "Klasörler",
    view_grid: "Izgara",
    view_list: "Liste",
    view_strip: "Şerit",
    view_shelf: "Raf",
};

pub fn ui() -> &'static Ui {
    match current() {
        Lang::En => &EN,
        Lang::Fr => &FR,
        Lang::Es => &ES,
        Lang::Ru => &RU,
        Lang::De => &DE,
        Lang::It => &IT,
        Lang::Pt => &PT,
        Lang::Zh => &ZH,
        Lang::Tr => &TR,
    }
}

/// "94 jeux", with each language's own plural rule (see `loc::games_count`).
pub fn games(n: usize) -> std::string::String {
    match current() {
        Lang::En if n == 1 => "1 game".into(),
        Lang::En => std::format!("{} games", n),
        Lang::Fr if n <= 1 => std::format!("{} jeu", n),
        Lang::Fr => std::format!("{} jeux", n),
        Lang::Es if n == 1 => "1 juego".into(),
        Lang::Es => std::format!("{} juegos", n),
        Lang::Ru => std::format!("Игр: {}", n),
        Lang::De if n == 1 => "1 Spiel".into(),
        Lang::De => std::format!("{} Spiele", n),
        Lang::It if n == 1 => "1 gioco".into(),
        Lang::It => std::format!("{} giochi", n),
        Lang::Pt if n == 1 => "1 jogo".into(),
        Lang::Pt => std::format!("{} jogos", n),
        Lang::Zh => std::format!("游戏：{}", n),
        Lang::Tr => std::format!("{} oyun", n),
    }
}

/// The layout's name, for the home header.
pub fn view_label(v: u8) -> &'static str {
    let u = ui();
    match v % 4 {
        1 => u.view_list,
        2 => u.view_strip,
        3 => u.view_shelf,
        _ => u.view_grid,
    }
}

/// Time played, written the way each language says it: "2 h 39 de jeu",
/// "2 h 39 played". Under an hour the hours are left out rather than shown as
/// "0 h".
pub fn playtime(secs: u64) -> std::string::String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    match current() {
        Lang::En if h == 0 => std::format!("{} min played", m),
        Lang::En => std::format!("{} h {:02} played", h, m),
        Lang::Fr if h == 0 => std::format!("{} min de jeu", m),
        Lang::Fr => std::format!("{} h {:02} de jeu", h, m),
        Lang::Es if h == 0 => std::format!("{} min jugados", m),
        Lang::Es => std::format!("{} h {:02} jugadas", h, m),
        Lang::Ru if h == 0 => std::format!("{} мин в игре", m),
        Lang::Ru => std::format!("{} ч {:02} мин в игре", h, m),
        Lang::De if h == 0 => std::format!("{} Min. gespielt", m),
        Lang::De => std::format!("{} Std. {:02} gespielt", h, m),
        Lang::It if h == 0 => std::format!("{} min di gioco", m),
        Lang::It => std::format!("{} h {:02} di gioco", h, m),
        Lang::Pt if h == 0 => std::format!("{} min jogados", m),
        Lang::Pt => std::format!("{} h {:02} jogadas", h, m),
        Lang::Zh if h == 0 => std::format!("已玩 {} 分钟", m),
        Lang::Zh => std::format!("已玩 {} 小时 {} 分钟", h, m),
        Lang::Tr if h == 0 => std::format!("{} dk oynandı", m),
        Lang::Tr => std::format!("{} sa {:02} dk oynandı", h, m),
    }
}
