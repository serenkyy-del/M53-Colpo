//! Application fields for the versioned colposcopy protocol.
use crate::protocol::Region;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Colpo {
    pub fields: BTreeMap<String, Answer>,
    pub marks: BTreeMap<Uuid, Mark>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mark_history: BTreeMap<String, Mark>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub asset_id: Uuid,
    pub catalog_id: String,
    pub properties: BTreeMap<String, Answer>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Answer {
    Text(String),
    Choice(String),
    Checked(bool),
}
impl Answer {
    pub fn checked(&self) -> bool {
        matches!(self, Self::Checked(true))
    }
    pub fn display(&self) -> String {
        match self {
            Self::Text(s) | Self::Choice(s) => s.clone(),
            Self::Checked(b) => if *b { "Да" } else { "Нет" }.into(),
        }
    }
}
impl Colpo {
    /// Calendar day of the cycle on the recorded examination date (no 28-day wrap).
    pub fn cycle_day(&self) -> Option<i64> {
        if ["context.pregnancy", "context.menopause"]
            .iter()
            .any(|id| self.fields.get(*id).is_some_and(Answer::checked))
        {
            return None;
        }
        let date = |id: &str| match self.fields.get(id) {
            Some(Answer::Text(value)) => calendar_day(value),
            _ => None,
        };
        let difference = date("context.exam_date")? - date("context.lmp")?;
        (difference >= 0).then_some(difference + 1)
    }

    pub fn hidden(&self) -> bool {
        ["context.no_cervix", "context.skip"]
            .iter()
            .any(|k| self.fields.get(*k).is_some_and(Answer::checked))
    }
}

// Strict Gregorian ISO date, shared by UI and report without timezone arithmetic.
fn calendar_day(value: &str) -> Option<i64> {
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || !value
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = value[0..4].parse().ok()?;
    let month: usize = value[5..7].parse().ok()?;
    let day: i64 = value[8..10].parse().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year == 0 || !(1..=12).contains(&month) || day < 1 || day > months[month - 1] {
        return None;
    }
    let previous = year - 1;
    Some(
        previous * 365 + previous / 4 - previous / 100
            + previous / 400
            + months[..month - 1].iter().sum::<i64>()
            + day,
    )
}
#[derive(Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub short: &'static str,
    pub name: &'static str,
    pub code: Option<&'static str>,
    pub region: Region,
    pub group: usize,
}
/// Display legend requested by the physician; not a severity/diagnostic score.
pub const MARKER_GROUPS: &[(&str, &str, u32)] = &[
    (
        "Повреждения и рубцы",
        "Разрывы и рубцовые образования",
        0x7e22ce,
    ),
    (
        "Эпителий и Йод(+)",
        "Эпителиальные обозначения и йодпозитивная проба",
        0x15803d,
    ),
    (
        "Изменения и Йод(-)",
        "АБЭ, лейкоплакия, эрозия и йоднегативная проба",
        0xdc2626,
    ),
    (
        "Атипические сосуды",
        "Отдельная группа сосудистых обозначений",
        0xdc2626,
    ),
    (
        "Другие находки",
        "Прочие обозначения обследования",
        0xc66a06,
    ),
    (
        "Нормальные находки вульвы",
        "Нормальные варианты из каталога обозначений",
        0x15803d,
    ),
    ("Типы образований", "Форма и тип образования", 0xc66a06),
    (
        "Поверхность и морфология",
        "Изменения поверхности из каталога обозначений",
        0xc66a06,
    ),
    (
        "Другие находки вульвы",
        "Прочие обозначения вульвы",
        0x7e22ce,
    ),
    (
        "Подозрительные признаки",
        "Обозначения, выбранные врачом",
        0xdc2626,
    ),
];
impl Preset {
    pub fn marker_group(&self) -> usize {
        if self.region == Region::Cervix {
            match self
                .id
                .rsplit('.')
                .next()
                .and_then(|n| n.parse::<usize>().ok())
            {
                Some(0..=1) => 0,
                Some(2..=6) => 1,
                Some(7..=11) => 2,
                Some(12) => 3,
                _ => 4,
            }
        } else if self.id.starts_with("vulva.normal.") {
            5
        } else if self.id.starts_with("vulva.type.") {
            6
        } else if self.id.starts_with("vulva.morph.") {
            7
        } else if self.id.starts_with("vulva.cancer.")
            || self.id.starts_with("vulva.magnification.")
        {
            9
        } else {
            8
        }
    }
    pub fn marker_color(&self) -> u32 {
        MARKER_GROUPS[self.marker_group()].2
    }
}
pub fn marker_color(label: &str, full_name: &str, term_code: Option<&str>) -> u32 {
    let label = label.split('·').next().unwrap_or(label);
    PRESETS
        .iter()
        .find(|p| p.short == label && p.name == full_name && p.code == term_code)
        .or_else(|| {
            PRESETS
                .iter()
                .find(|p| p.short == label && p.name == full_name)
        })
        .map(Preset::marker_color)
        .unwrap_or(0xc66a06)
}
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "cervix.symbol.0",
        short: "Р",
        name: "Разрывы",
        code: None,
        region: Region::Cervix,
        group: 0,
    },
    Preset {
        id: "cervix.symbol.1",
        short: "РО",
        name: "Рубцовые образования",
        code: None,
        region: Region::Cervix,
        group: 0,
    },
    Preset {
        id: "cervix.symbol.2",
        short: "МПЭ",
        name: "Многослойный плоский эпителий",
        code: None,
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.3",
        short: "ЦЭ",
        name: "Цилиндрический эпителий",
        code: Some("cervix.normal.columnar_epithelium"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.4",
        short: "МЭ",
        name: "Метапластический эпителий",
        code: Some("cervix.normal.metaplastic_squamous_epithelium"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.5",
        short: "Д",
        name: "Децидуоз",
        code: Some("cervix.normal.deciduosis"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.6",
        short: "Йод(+)",
        name: "Проба Шиллера йодпозитивная",
        code: None,
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.7",
        short: "АБЭ I",
        name: "Ацетобелый эпителий 1-й степени",
        code: Some("cervix.minor.thin_acetowhite_epithelium"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.8",
        short: "АБЭ II",
        name: "Ацетобелый эпителий 2-й степени",
        code: Some("cervix.major.dense_acetowhite_epithelium"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.9",
        short: "Л",
        name: "Лейкоплакия",
        code: Some("cervix.nonspecific.leukoplakia"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.10",
        short: "Э",
        name: "Эрозия",
        code: Some("cervix.nonspecific.erosion"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.11",
        short: "Йод(-)",
        name: "Проба Шиллера йоднегативная",
        code: None,
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.12",
        short: "АС",
        name: "Атипические сосуды",
        code: Some("cervix.invasion.atypical_vessels"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.13",
        short: "ВЗТ",
        name: "Врожденная зона трансформации",
        code: Some("cervix.other.congenital_transformation_zone"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.14",
        short: "К",
        name: "Кондиломы",
        code: Some("cervix.other.condyloma"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.15",
        short: "С",
        name: "Стеноз",
        code: Some("cervix.other.stenosis"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.16",
        short: "ВА",
        name: "Врожденные аномалии",
        code: Some("cervix.other.congenital_anomaly"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.17",
        short: "В",
        name: "Воспаление",
        code: Some("cervix.other.inflammation"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.18",
        short: "П-П",
        name: "Полипы",
        code: Some("cervix.other.polyp"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "cervix.symbol.19",
        short: "Э-З",
        name: "Эндометриоз",
        code: Some("cervix.other.endometriosis"),
        region: Region::Cervix,
        group: 2,
    },
    Preset {
        id: "vulva.normal.micro",
        short: "М",
        name: "Микропапилломатоз",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.normal.fordyce",
        short: "СЖ",
        name: "Сальные железы (точки Фордица)",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.normal.redness",
        short: "ПП",
        name: "Покраснение преддверия",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.spot",
        short: "Пя/Пш",
        name: "Пятна / пятнышки",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.papule",
        short: "Пап",
        name: "Папула",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.nodule",
        short: "У",
        name: "Узелок преддверия",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.cyst",
        short: "К",
        name: "Киста",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.pustule",
        short: "Пус",
        name: "Пустула",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.plaque",
        short: "Бл",
        name: "Бляшка",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.vesicle",
        short: "Пуз",
        name: "Пузырек",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.type.bulla",
        short: "Пузк",
        name: "Пузырь",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.eczema",
        short: "Экз",
        name: "Экзема",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.lichen",
        short: "Л",
        name: "Лихенификация",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.excoriation",
        short: "Э",
        name: "Экскориация",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.purpura",
        short: "Пур",
        name: "Пурпура",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.scarification",
        short: "С",
        name: "Скарификация",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.fissure",
        short: "Т",
        name: "Трещина",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.ulcer",
        short: "Я",
        name: "Язва",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.erosion",
        short: "Э",
        name: "Эрозия",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.morph.wart",
        short: "Б",
        name: "Бородавка",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.other.trauma",
        short: "Тр",
        name: "Травма",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.other.malformation",
        short: "ПР",
        name: "Порок развития",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.other.hematoma",
        short: "Г",
        name: "Гематома",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.other.hemangioma",
        short: "Га",
        name: "Гемангиома",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.neoplasia",
        short: "Нео",
        name: "Явная неоплазия",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.ulceration",
        short: "Из",
        name: "Изъязвление",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.necrosis",
        short: "Нек",
        name: "Некроз",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.bleeding",
        short: "Кр",
        name: "Кровоточивость",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.hyperkeratosis",
        short: "ГК",
        name: "Гиперкератоз",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.cancer.exophytic",
        short: "Эф",
        name: "Экзофит",
        code: None,
        region: Region::Vulva,
        group: 0,
    },
    Preset {
        id: "vulva.magnification.aceto",
        short: "АБЭ",
        name: "Ацетобелый эпителий",
        code: None,
        region: Region::Vulva,
        group: 1,
    },
    Preset {
        id: "vulva.magnification.punctation",
        short: "П",
        name: "Пунктация",
        code: None,
        region: Region::Vulva,
        group: 1,
    },
    Preset {
        id: "vulva.magnification.vessels",
        short: "АтС",
        name: "Атипические сосуды",
        code: None,
        region: Region::Vulva,
        group: 1,
    },
    Preset {
        id: "vulva.magnification.surface",
        short: "НП",
        name: "Нерегулярная поверхность",
        code: None,
        region: Region::Vulva,
        group: 1,
    },
    Preset {
        id: "vulva.magnification.analjunction",
        short: "ААСЭ",
        name: "Аномальный анальный стык эпителиев (уточнение локализации зубчатой линии)",
        code: None,
        region: Region::Anus,
        group: 1,
    },
];
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}
#[derive(Clone, Copy, PartialEq)]
pub enum Control {
    Text,
    Date,
    Choice,
    Check,
    Unknown,
}
#[derive(Clone, Copy)]
pub struct Field {
    pub id: &'static str,
    pub label: &'static str,
    pub control: Control,
    pub choices: &'static [&'static str],
    pub group: usize,
    pub parent: &'static str,
}
pub const FIELDS: &[Field] = &[
    Field {
        id: "vulva.skin.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 5,
        parent: "",
    },
    Field {
        id: "vulva.skin.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 5,
        parent: "",
    },
    Field {
        id: "vulva.skin.dark",
        label: "Темный",
        control: Control::Check,
        choices: &[],
        group: 5,
        parent: "",
    },
    Field {
        id: "vulva.other_comment",
        label: "Дополнительно · Различные находки",
        control: Control::Text,
        choices: &[],
        group: 5,
        parent: "",
    },
    Field {
        id: "vulva.cancer_comment",
        label: "Дополнительно · Подозрение на рак",
        control: Control::Text,
        choices: &[],
        group: 5,
        parent: "",
    },
    Field {
        id: "context.exam_date",
        label: "Дата обследования",
        control: Control::Date,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.lmp",
        label: "Дата последней менструации",
        control: Control::Date,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.phase",
        label: "Фаза цикла",
        control: Control::Choice,
        choices: &["Нет", "I", "II"],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.pregnancy",
        label: "Беременность",
        control: Control::Check,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.menopause",
        label: "Постменопауза",
        control: Control::Check,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.no_cervix",
        label: "Шейка матки удалена",
        control: Control::Check,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "context.skip",
        label: "Не заполнять данные кольпоскопии",
        control: Control::Check,
        choices: &[],
        group: 4,
        parent: "",
    },
    Field {
        id: "cervix.shape",
        label: "Форма",
        control: Control::Choice,
        choices: &["Цилиндрическая", "Коническая"],
        group: 0,
        parent: "",
    },
    Field {
        id: "cervix.hypertrophy",
        label: "Гипертрофия",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "",
    },
    Field {
        id: "cervix.os",
        label: "Зев",
        control: Control::Choice,
        choices: &["Округлый", "Щелевидный"],
        group: 0,
        parent: "",
    },
    Field {
        id: "cervix.surface",
        label: "Поверхность слизистой",
        control: Control::Choice,
        choices: &["Блестящая", "Тусклая", "Атрофичная"],
        group: 0,
        parent: "",
    },
    Field {
        id: "cervix.color",
        label: "Цвет слизистой",
        control: Control::Choice,
        choices: &["БледноРозовая", "Гиперемированная", "Синюшная"],
        group: 0,
        parent: "",
    },
    Field {
        id: "simple.relief",
        label: "Рельеф",
        control: Control::Choice,
        choices: &["Ровный", "Неровный"],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.vessels",
        label: "Сосудистый рисунок",
        control: Control::Choice,
        choices: &["Отсутствует", "Норма", "Усиление"],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.closed",
        label: "Закрытые железы",
        control: Control::Check,
        choices: &[],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.closed_count",
        label: "Количество: закрытые железы",
        control: Control::Choice,
        choices: &["Единичные", "Множественные"],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.open",
        label: "Открытые железы",
        control: Control::Check,
        choices: &[],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.open_count",
        label: "Количество: открытые железы",
        control: Control::Choice,
        choices: &["Единичные", "Множественные"],
        group: 1,
        parent: "",
    },
    Field {
        id: "simple.transition",
        label: "Переходная зона",
        control: Control::Choice,
        choices: &["Видна", "НеВидна"],
        group: 1,
        parent: "",
    },
    Field {
        id: "extended.adequacy",
        label: "Кольпоскопическая картина",
        control: Control::Choice,
        choices: &["Адекватная", "Неадекватная"],
        group: 2,
        parent: "",
    },
    Field {
        id: "extended.tz",
        label: "Зона трансформации",
        control: Control::Choice,
        choices: &["ЗТ_I", "ЗТ_II", "ЗТ_III"],
        group: 2,
        parent: "",
    },
    Field {
        id: "extended.location",
        label: "Локализация поражения",
        control: Control::Choice,
        choices: &["В пределах зоны трансформации", "Вне зоны трансформации"],
        group: 2,
        parent: "",
    },
    Field {
        id: "extended.clock",
        label: "Соответствие с циферблатом",
        control: Control::Text,
        choices: &[],
        group: 2,
        parent: "",
    },
    Field {
        id: "extended.size",
        label: "Размеры области поражения",
        control: Control::Choice,
        choices: &[
            "<¼ цервикса",
            "¼ цервикса",
            "<½ цервикса",
            "½ цервикса",
            ">½ цервикса",
            "¾ цервикса",
            ">¾ цервикса",
        ],
        group: 2,
        parent: "",
    },
    Field {
        id: "extended.other",
        label: "Прочие",
        control: Control::Text,
        choices: &[],
        group: 2,
        parent: "",
    },
    Field {
        id: "rectovaginal.description",
        label: "Ректо-вагинальный осмотр",
        control: Control::Text,
        choices: &[],
        group: 3,
        parent: "",
    },
    Field {
        id: "property.mature",
        label: "Зрелый",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.2",
    },
    Field {
        id: "property.atrophic",
        label: "Атрофический",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.2",
    },
    Field {
        id: "property.closed",
        label: "Закрытые железы",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.4",
    },
    Field {
        id: "property.open",
        label: "Открытые железы",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.4",
    },
    Field {
        id: "property.fine_mosaic",
        label: "Нежная мозаика",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.7",
    },
    Field {
        id: "property.fine_punctation",
        label: "Нежная пунктация",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.7",
    },
    Field {
        id: "property.rapid",
        label: "Быстро появляется",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.8",
    },
    Field {
        id: "property.persistent",
        label: "Долго держится",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.8",
    },
    Field {
        id: "property.coarse_mosaic",
        label: "Грубая мозаика",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.8",
    },
    Field {
        id: "property.coarse_punctation",
        label: "Грубая пунктация",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.8",
    },
    Field {
        id: "property.fragile",
        label: "Ломкие сосуды",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.surface",
        label: "Неровная поверхность",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.exophytic",
        label: "Экзофитные поражения",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.necrosis_ulceration",
        label: "Область некроза, изъязвления",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.crypt_rim",
        label: "Ацетобелый плотный ободок вокруг открытых желез (крипт)",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.ridge",
        label: "Признак бугристости",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "property.inner_border",
        label: "Внутри поражения - контуры более плотного ацетобелого участка",
        control: Control::Check,
        choices: &[],
        group: 2,
        parent: "cervix.symbol.12",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.spot",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.spot",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.spot",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.papule",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.papule",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.papule",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.nodule",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.nodule",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.nodule",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.cyst",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.cyst",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.cyst",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.pustule",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.pustule",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.pustule",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.plaque",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.plaque",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.plaque",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.vesicle",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.vesicle",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.vesicle",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.bulla",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.bulla",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.type.bulla",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.eczema",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.eczema",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.eczema",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.lichen",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.lichen",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.lichen",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.excoriation",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.excoriation",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.excoriation",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.purpura",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.purpura",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.purpura",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.scarification",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.scarification",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.scarification",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.fissure",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.fissure",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.fissure",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.ulcer",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.ulcer",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.ulcer",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.erosion",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.erosion",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.erosion",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.wart",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.wart",
    },
    Field {
        id: "color.dark",
        label: "Тёмный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.morph.wart",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.other.trauma",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.other.malformation",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.other.hematoma",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.other.hemangioma",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.neoplasia",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.ulceration",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.necrosis",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.bleeding",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.hyperkeratosis",
    },
    Field {
        id: "color.white",
        label: "Белый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
    Field {
        id: "color.grey",
        label: "Серый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
    Field {
        id: "color.red",
        label: "Красный",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
    Field {
        id: "color.brown",
        label: "Коричневый",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
    Field {
        id: "color.no_color",
        label: "Без цвета",
        control: Control::Check,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
    Field {
        id: "comment",
        label: "Дополнительно",
        control: Control::Text,
        choices: &[],
        group: 0,
        parent: "vulva.cancer.exophytic",
    },
];
pub fn validate_value(field: &Field, answer: &Answer) -> bool {
    match (&field.control, answer) {
        (Control::Check, Answer::Checked(_)) => true,
        (Control::Choice, Answer::Choice(v)) => field.choices.contains(&v.as_str()),
        (Control::Text, Answer::Text(v)) => v.len() <= 16384 && !v.contains('\0'),
        (Control::Date, Answer::Text(v)) => valid_date(v),
        _ => false,
    }
}
fn valid_date(value: &str) -> bool {
    calendar_day(value).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchillerResult {
    Positive,
    Negative,
}
impl Colpo {
    pub fn schiller(&self, asset: Uuid) -> Option<SchillerResult> {
        self.marks
            .values()
            .find(|m| {
                m.asset_id == asset
                    && matches!(
                        m.catalog_id.as_str(),
                        "cervix.symbol.6" | "cervix.symbol.11"
                    )
            })
            .map(|m| {
                if m.catalog_id == "cervix.symbol.6" {
                    SchillerResult::Positive
                } else {
                    SchillerResult::Negative
                }
            })
    }
    pub fn errors(&self) -> Vec<String> {
        let mut errors = vec![];
        for (id, value) in &self.fields {
            if matches!(id.as_str(), "extended.adequacy" | "extended.tz") {
                errors.push("assessment_must_use_typed_field".into());
            }
            let legacy_acetowhite = matches!(id.as_str(), "cervix.symbol.7" | "cervix.symbol.8")
                && matches!(value, Answer::Checked(_));
            if !legacy_acetowhite && !FIELDS
                .iter()
                .any(|f| f.parent.is_empty() && f.id == id && validate_value(f, value))
            {
                errors.push(format!("fields.{id}: invalid_value"));
            }
        }
        let mut iodine = BTreeMap::new();
        for (id, mark) in &self.marks {
            if id.is_nil() || mark.asset_id.is_nil() || preset(&mark.catalog_id).is_none() {
                errors.push("invalid_mark".into());
            }
            for (key, value) in &mark.properties {
                if !FIELDS
                    .iter()
                    .any(|f| f.parent == mark.catalog_id && f.id == key && validate_value(f, value))
                {
                    errors.push(format!("marks.{id}.{key}: invalid_property"));
                }
            }
            if matches!(
                mark.catalog_id.as_str(),
                "cervix.symbol.6" | "cervix.symbol.11"
            ) && iodine
                .insert(mark.asset_id, &mark.catalog_id)
                .is_some_and(|old| old != &mark.catalog_id)
            {
                errors.push("conflicting_schiller_results".into());
            }
        }
        errors
    }
}

pub fn subgroup(p: &Preset) -> (&'static str, usize) {
    if p.region == Region::Cervix {
        return match p.id {
            "cervix.symbol.0" | "cervix.symbol.1" => ("Зев", 0),
            "cervix.symbol.2" | "cervix.symbol.3" | "cervix.symbol.4" | "cervix.symbol.5" => {
                ("Нормальные кольпоскопические картины", 0)
            }
            "cervix.symbol.7" | "cervix.symbol.8" => ("Аномальные кольпоскопические картины", 1),
            "cervix.symbol.9" | "cervix.symbol.10" => ("Неспецифические признаки", 2),
            "cervix.symbol.6" | "cervix.symbol.11" => ("Проба Шиллера", 3),
            "cervix.symbol.12" => ("Подозрение на инвазию", 4),
            _ => ("Другие кольпоскопические картины", 5),
        };
    }
    if p.id.contains(".normal.") {
        ("Нормальные картины", 0)
    } else if p.id.contains(".type.") {
        ("Анормальные картины · Тип поражения", 1)
    } else if p.id.contains(".morph.") {
        ("Вторичная морфология", 2)
    } else if p.id.contains(".other.") {
        ("Различные находки", 3)
    } else if p.id.contains(".cancer.") {
        ("Подозрение на рак", 4)
    } else {
        (
            "Анормальные кольпоскопические или другие картины при увеличении",
            0,
        )
    }
}
