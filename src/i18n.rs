//! User-facing text: English by default, Russian when the locale asks for
//! it (`LANGUAGE`, then `LC_ALL` / `LC_MESSAGES` / `LANG`, as gettext does).
//!
//! The catalog is keyed by the English text itself, so English never goes
//! missing and a message without a translation simply stays English.
//! Placeholders are positional `{}`; [`t!`](crate::t) fills them.
//! A test checks that every `t!("...")` in the sources has a Russian entry
//! with the same number of placeholders.

use std::cell::Cell;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

static PROCESS: AtomicU8 = AtomicU8::new(0);
thread_local! {
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

fn code(tag: &str) -> Option<Lang> {
    let tag = tag.trim().to_ascii_lowercase();
    if tag.is_empty() {
        None
    } else if tag == "ru"
        || tag.starts_with("ru_")
        || tag.starts_with("ru.")
        || tag.starts_with("ru@")
    {
        Some(Lang::Ru)
    } else {
        Some(Lang::En)
    }
}

impl Lang {
    /// The message language for an environment, following gettext:
    /// `LANGUAGE` (a `:`-separated priority list) wins unless the locale is
    /// `C`/`POSIX`; the locale is `LC_ALL`, else `LC_MESSAGES`, else `LANG`.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Lang {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| get(k).filter(|v| !v.is_empty()));
        if matches!(locale.as_deref(), Some("C") | Some("POSIX"))
            || locale.as_deref().is_some_and(|l| l.starts_with("C."))
        {
            return Lang::En;
        }
        if let Some(list) = get("LANGUAGE")
            && let Some(l) = list.split(':').find_map(code)
        {
            return l;
        }
        locale.as_deref().and_then(code).unwrap_or(Lang::En)
    }
}

/// Set the process language from the environment (called once by `main`).
pub fn init_from_env() {
    let env = |k: &str| std::env::var(k).ok();
    set(Lang::from_env(&env));
}

pub fn set(lang: Lang) {
    PROCESS.store(lang as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    OVERRIDE
        .with(|o| o.get())
        .unwrap_or(match PROCESS.load(Ordering::Relaxed) {
            1 => Lang::Ru,
            _ => Lang::En,
        })
}

/// Run `f` with `lang` on this thread only (tests run in parallel).
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let prev = OVERRIDE.with(|o| o.replace(Some(lang)));
    let r = f();
    OVERRIDE.with(|o| o.set(prev));
    r
}

pub fn tr(msgid: &'static str) -> &'static str {
    tr_in(current(), msgid)
}

pub fn tr_in(lang: Lang, msgid: &'static str) -> &'static str {
    match lang {
        Lang::En => msgid,
        Lang::Ru => RU
            .iter()
            .find(|(en, _)| *en == msgid)
            .map(|(_, ru)| *ru)
            .unwrap_or(msgid),
    }
}

/// Replace each `{}` in `template`, in order.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// `t!("English text")` or `t!("Text with {} and {}", a, b)`: translated
/// for the current language.
#[macro_export]
macro_rules! t {
    ($msg:literal) => {
        $crate::i18n::tr($msg)
    };
    ($msg:literal, $($arg:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($msg), &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// English → Russian.
pub static RU: &[(&str, &str)] = &[
    // --- runners -----------------------------------------------------------
    (
        "Wine (direct execution on x86-64)",
        "Wine (прямое выполнение на x86-64)",
    ),
    (
        "Wine ARM64EC/WoW64 + FEX (x86 → ARM translation)",
        "Wine ARM64EC/WoW64 + FEX (трансляция x86 → ARM)",
    ),
    (
        "x86-64 Wine under FEXInterpreter (x86 → ARM translation)",
        "Wine x86-64 под FEXInterpreter (трансляция x86 → ARM)",
    ),
    (
        "Wine ARM64 (native ARM execution)",
        "Wine ARM64 (нативное выполнение на ARM)",
    ),
    (
        "Lucent Bridge supports x86-64 and AArch64 computers only.",
        "Lucent Bridge работает только на компьютерах x86-64 и AArch64.",
    ),
    (
        "Preferred runner '{}' is not available or not applicable here; using '{}'.",
        "Предпочтительный способ запуска «{}» здесь недоступен или не подходит; используется «{}».",
    ),
    (
        "This is a Windows-on-ARM program ({}). x86-64 computers cannot execute ARM code, and Lucent Bridge does not translate ARM to x86.",
        "Это программа для Windows на ARM ({}). Компьютеры x86-64 не выполняют код ARM, а Lucent Bridge не переводит ARM в x86.",
    ),
    (
        "Download the x64 (or x86) version of the program instead.",
        "Скачайте версию программы для x64 (или x86).",
    ),
    (
        "This program was built for a CPU Lucent Bridge does not support (machine type {}, e.g. Itanium, MIPS or 32-bit ARM).",
        "Эта программа собрана для процессора, который Lucent Bridge не поддерживает (тип машины {}, например Itanium, MIPS или 32-битный ARM).",
    ),
    ("Wine is not installed.", "Wine не установлен."),
    (
        "Install it with: rpm-ostree install wine-core wine-mono (then reboot).",
        "Установите его: rpm-ostree install wine-core wine-mono (затем перезагрузите компьютер).",
    ),
    (
        "This is a 32-bit Windows program, but the installed Wine has no 32-bit (i386) support.",
        "Это 32-битная программа Windows, но установленный Wine не поддерживает 32-битный код (i386).",
    ),
    (
        "Install a Wine build with WoW64 support (Fedora's wine-core package has it).",
        "Установите сборку Wine с поддержкой WoW64 (она есть в пакете wine-core из Fedora).",
    ),
    (
        "This is a 64-bit Windows program, but the installed Wine has no x86-64 Windows DLLs.",
        "Это 64-битная программа Windows, но в установленном Wine нет библиотек Windows для x86-64.",
    ),
    (
        "Reinstall Wine: rpm-ostree install wine-core",
        "Переустановите Wine: rpm-ostree install wine-core",
    ),
    (
        "The installed Wine has no x86 Windows DLLs to run msiexec.",
        "В установленном Wine нет библиотек Windows для x86, чтобы запустить msiexec.",
    ),
    ("an ARM64 Wine", "Wine для ARM64"),
    (
        "an ARM64 Wine (the installed one is not)",
        "Wine для ARM64 (установленный Wine собран не для ARM64)",
    ),
    (
        "a Wine built with ARM64EC support",
        "Wine с поддержкой ARM64EC",
    ),
    (
        "Wine's 32-bit (i386) PE DLLs",
        "32-битные (i386) PE-библиотеки Wine",
    ),
    ("FEX's {}", "{} из FEX"),
    (
        "Recommended: install {} (packages wine-core and fex-emu-wine).",
        "Рекомендуется: установите {} (пакеты wine-core и fex-emu-wine).",
    ),
    ("FEX-Emu (FEXInterpreter)", "FEX-Emu (FEXInterpreter)"),
    (
        "an x86-64 RootFS that contains Wine",
        "x86-64 RootFS с Wine внутри",
    ),
    ("Alternative: install {}.", "Другой вариант: установите {}."),
    (" and ", " и "),
    (
        "No x86-64 emulator (libarm64ecfex.dll): 64-bit x86 programs started by this one will fail.",
        "Нет эмулятора x86-64 (libarm64ecfex.dll): 64-битные x86-программы, которые запустит эта программа, не заработают.",
    ),
    (
        "No 32-bit x86 emulator (libwow64fex.dll): 32-bit programs started by this one will fail.",
        "Нет эмулятора 32-битного x86 (libwow64fex.dll): 32-битные программы, которые запустит эта программа, не заработают.",
    ),
    (
        "Fallback path: all of Wine runs translated, so expect lower performance than the ARM64EC path.",
        "Запасной путь: весь Wine выполняется через трансляцию, поэтому работать будет медленнее, чем через ARM64EC.",
    ),
    (
        "This kernel uses {} KiB memory pages, but Wine and FEX need 4 KiB pages to run Windows programs. The usual workaround (on Apple-silicon Macs, for example) is a micro-VM, and Lucent Bridge does not use virtual machines.",
        "Это ядро использует страницы памяти по {} КиБ, а Wine и FEX нужны страницы по 4 КиБ. Обычный обходной путь (например, на Mac с Apple silicon) — микро-ВМ, а Lucent Bridge виртуальные машины не использует.",
    ),
    (
        "Boot a kernel built with 4 KiB pages, if your hardware supports one.",
        "Загрузите ядро со страницами по 4 КиБ, если ваше оборудование это позволяет.",
    ),
    (
        "This ARM64 Windows program needs an ARM64 Wine, but the installed Wine is not an ARM64 build.",
        "Этой программе Windows для ARM64 нужен Wine для ARM64, а установленный Wine собран не для ARM64.",
    ),
    (
        "Install Wine for AArch64: rpm-ostree install wine-core",
        "Установите Wine для AArch64: rpm-ostree install wine-core",
    ),
    (
        "ARM64EC programs may contain x86-64 code; without libarm64ecfex.dll those parts will fail.",
        "Программы ARM64EC могут содержать код x86-64; без libarm64ecfex.dll эти части не заработают.",
    ),
    (
        "This ARM64EC program needs a Wine built with ARM64EC support.",
        "Этой программе ARM64EC нужен Wine с поддержкой ARM64EC.",
    ),
    (
        "Install an ARM64EC-enabled Wine build (Wine 10 or newer, built with --enable-archs=arm64ec,aarch64,i386).",
        "Установите сборку Wine с ARM64EC (Wine 10 или новее, собранный с --enable-archs=arm64ec,aarch64,i386).",
    ),
    (
        "No x86 emulator is installed: msiexec runs natively, but x86 custom actions in this package (and the installed program, if it is x86) will fail.",
        "Эмулятор x86 не установлен: msiexec работает нативно, но x86-действия этого пакета (и установленная программа, если она для x86) не заработают.",
    ),
    (
        "This is a Windows Installer package.",
        "Это пакет установщика Windows (MSI).",
    ),
    (
        "This is an {} Windows program.",
        "Это программа Windows для {}.",
    ),
    (
        "{} Running it on this ARM computer needs x86 → ARM translation, and no complete translation setup is installed.",
        "{} Чтобы запустить её на этом ARM-компьютере, нужна трансляция x86 → ARM, а полный набор для трансляции не установлен.",
    ),
    // --- plans -------------------------------------------------------------
    (
        "This is a DLL (a library), not a program that can be started.",
        "Это DLL (библиотека), а не программа, которую можно запустить.",
    ),
    (
        ".NET program: Wine runs it with Wine Mono; if it needs Microsoft .NET, add a dotnet verb to its compat entry.",
        "Программа .NET: Wine запускает её через Wine Mono; если ей нужен Microsoft .NET, добавьте компонент dotnet в её запись совместимости.",
    ),
    (
        "A native Linux version is available ({}).",
        "Есть версия для Linux ({}).",
    ),
    ("{} is known not to work.", "Известно, что {} не работает."),
    (
        "Create the Windows environment (first launch only)",
        "Создание среды Windows (только при первом запуске)",
    ),
    (
        "Copy the prebuilt Windows environment (first launch only)",
        "Копирование готовой среды Windows (только при первом запуске)",
    ),
    (
        "Create the Windows environment (only if copying it failed)",
        "Создание среды Windows (только если копирование не удалось)",
    ),
    ("Install components: {}", "Установка компонентов: {}"),
    (
        "winetricks is not installed; skipping recommended components: {}.",
        "winetricks не установлен; рекомендуемые компоненты пропущены: {}.",
    ),
    (
        "Recommended components ({}) are not applied automatically on the {} runner.",
        "Рекомендуемые компоненты ({}) не устанавливаются автоматически при запуске через {}.",
    ),
    (
        "--silent ignored: this is not an installer.",
        "--silent не учтён: это не установщик.",
    ),
    (
        "Lucent Bridge doesn't know how to run this installer ({}) silently.",
        "Lucent Bridge не знает, как запустить этот установщик ({}) без диалогов.",
    ),
    (
        "Run it without --silent and click through the installer.",
        "Запустите его без --silent и пройдите шаги установщика.",
    ),
    (
        "Or add silent_args for it to a compatibility entry.",
        "Или добавьте для него silent_args в запись совместимости.",
    ),
    ("Start {}", "Запуск {}"),
    (
        "Wait for the installer to finish completely",
        "Ожидание полного завершения установщика",
    ),
    (
        "Add the installed program to the application launcher",
        "Добавление установленной программы в меню приложений",
    ),
    ("Program:", "Программа:"),
    ("Type:", "Тип:"),
    (", installer", ", установщик"),
    ("Host:", "Компьютер:"),
    ("Runner:", "Запуск:"),
    ("none", "нет"),
    ("Prefix:", "Префикс:"),
    (" (will be created)", " (будет создан)"),
    ("Compat:", "Совместимость:"),
    (
        "{} (matched by {}, status: {}, from {})",
        "{} (найдено по: {}, статус: {}, источник: {})",
    ),
    ("SHA-256", "SHA-256"),
    ("product name", "название продукта"),
    ("file name", "имя файла"),
    ("works", "работает"),
    ("partial", "частично"),
    ("broken", "не работает"),
    ("unknown", "неизвестно"),
    (
        "Lucent Bridge plan for {} [{}]",
        "План Lucent Bridge для {} [{}]",
    ),
    ("Cannot run: {}", "Запуск невозможен: {}"),
    (
        "Blocked by the compatibility database: {}",
        "Заблокировано базой совместимости: {}",
    ),
    (
        "(use `run --force` to try anyway)",
        "(чтобы всё равно попробовать, используйте `run --force`)",
    ),
    ("Steps:", "Шаги:"),
    ("Notes:", "Примечания:"),
    // --- running -----------------------------------------------------------
    ("cannot open {}", "не удаётся открыть {}"),
    ("Can't run {}", "Не удаётся запустить {}"),
    ("{} won't work", "{} не будет работать"),
    (
        "(use --force to try anyway)",
        "(чтобы всё равно попробовать, используйте --force)",
    ),
    (
        "Windows environment ready in {} s (from template)",
        "Среда Windows готова за {} с (из шаблона)",
    ),
    (
        "the prefix template didn't work ({}); creating a new environment",
        "шаблон префикса не подошёл ({}); создаётся новая среда",
    ),
    ("Setting up {}", "Подготовка {}"),
    (
        "Preparing a Windows environment. This happens once and takes a minute.",
        "Готовим среду Windows. Это происходит один раз и занимает около минуты.",
    ),
    (
        "wineboot exited with {}; continuing",
        "wineboot завершился с кодом {}; продолжаем",
    ),
    (
        "Windows environment ready in {} s (wineboot)",
        "Среда Windows готова за {} с (wineboot)",
    ),
    (
        "winetricks failed ({}); launching anyway",
        "winetricks завершился с ошибкой ({}); всё равно запускаем",
    ),
    (
        "the installer created no shortcuts",
        "установщик не создал ярлыков",
    ),
    ("launcher entries added: {}", "добавлено ярлыков в меню: {}"),
    ("{} is installed", "{} установлена"),
    (
        "You can find it in the application launcher.",
        "Её можно найти в меню приложений.",
    ),
    ("Windows program", "Программа Windows"),
    // --- command line ------------------------------------------------------
    ("note: {}", "примечание: {}"),
    ("warning: {}", "предупреждение: {}"),
    ("Can't open this file", "Не удаётся открыть этот файл"),
    ("this file has no icon", "у этого файла нет значка"),
    (
        "unknown host architecture '{}' (use x86_64 or aarch64)",
        "неизвестная архитектура «{}» (используйте x86_64 или aarch64)",
    ),
    (
        "(This is not a Linux system; add --assume-tools [--host aarch64] to see the plan for a stock Lucent OS install.)",
        "(Это не Linux; добавьте --assume-tools [--host aarch64], чтобы увидеть план для стандартной Lucent OS.)",
    ),
    (
        "`run` only works on Linux; use `plan` for a dry run",
        "`run` работает только в Linux; для пробного прогона используйте `plan`",
    ),
    (
        "No apps yet. Double-click a .exe or run `lucent-bridge run <file.exe>`.",
        "Программ пока нет. Дважды щёлкните файл .exe или выполните `lucent-bridge run <файл.exe>`.",
    ),
    ("APP ID", "ID ПРОГРАММЫ"),
    ("NAME", "НАЗВАНИЕ"),
    ("RUNNER", "ЗАПУСК"),
    ("LAUNCHERS", "ЯРЛЫКИ"),
    (
        "This will permanently delete:",
        "Будет безвозвратно удалено:",
    ),
    (
        "{} (the app and all its files and settings)",
        "{} (программа и все её файлы и настройки)",
    ),
    (
        "refusing to delete without confirmation; pass --yes",
        "без подтверждения ничего не удаляется; добавьте --yes",
    ),
    ("Continue? [y/N] ", "Продолжить? [y/N] "),
    ("Nothing removed.", "Ничего не удалено."),
    ("Removed {}.", "Удалено: {}."),
    (
        "`build-template` only works on Linux",
        "`build-template` работает только в Linux",
    ),
    (
        "Built prefix template for {} in {} ({} s)",
        "Шаблон префикса для {} создан в {} ({} с)",
    ),
    (
        "no app with ID '{}' (see `lucent-bridge list`)",
        "нет программы с ID «{}» (см. `lucent-bridge list`)",
    ),
    (
        "'{}' is not a valid app ID (see `lucent-bridge list`)",
        "«{}» — неправильный ID программы (см. `lucent-bridge list`)",
    ),
    (
        "No shortcuts found in this app's prefix.",
        "В префиксе этой программы не найдено ярлыков.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn language_from_locale_like_gettext() {
        assert_eq!(Lang::from_env(&env(&[])), Lang::En);
        assert_eq!(Lang::from_env(&env(&[("LANG", "ru_RU.UTF-8")])), Lang::Ru);
        assert_eq!(Lang::from_env(&env(&[("LANG", "en_US.UTF-8")])), Lang::En);
        assert_eq!(
            Lang::from_env(&env(&[
                ("LANG", "ru_RU.UTF-8"),
                ("LC_MESSAGES", "en_GB.UTF-8")
            ])),
            Lang::En
        );
        assert_eq!(
            Lang::from_env(&env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "ru_UA.UTF-8")])),
            Lang::Ru
        );
        assert_eq!(
            Lang::from_env(&env(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "ru:en")])),
            Lang::Ru
        );
        // LANGUAGE is ignored in the C locale.
        assert_eq!(
            Lang::from_env(&env(&[("LC_ALL", "C"), ("LANGUAGE", "ru")])),
            Lang::En
        );
        assert_eq!(Lang::from_env(&env(&[("LANG", "rue_SK")])), Lang::En);
    }

    #[test]
    fn english_and_russian() {
        with_lang(Lang::En, || {
            assert_eq!(
                crate::t!("Setting up {}", "Notepad++"),
                "Setting up Notepad++"
            );
            assert_eq!(crate::t!("Nothing removed."), "Nothing removed.");
        });
        with_lang(Lang::Ru, || {
            assert_eq!(
                crate::t!("Setting up {}", "Notepad++"),
                "Подготовка Notepad++"
            );
            assert_eq!(crate::t!("Nothing removed."), "Ничего не удалено.");
            // A message without a translation stays English.
            assert_eq!(tr("not in the catalog"), "not in the catalog");
        });
        assert_eq!(current(), Lang::En, "override is scoped");
    }

    #[test]
    fn real_messages_in_russian() {
        use crate::arch::{ExeArch, HostArch};
        use crate::runner::{Selection, Toolset, select};
        let sel = with_lang(Lang::Ru, || {
            select(HostArch::X86_64, ExeArch::X86_64, &Toolset::default(), None)
        });
        match sel {
            Selection::Unsupported(u) => {
                assert_eq!(u.reason, "Wine не установлен.");
                assert!(u.hints[0].starts_with("Установите его"));
            }
            Selection::Run(_) => panic!("no Wine, no runner"),
        }
        let sel = select(HostArch::X86_64, ExeArch::X86_64, &Toolset::default(), None);
        let Selection::Unsupported(u) = sel else {
            panic!()
        };
        assert_eq!(u.reason, "Wine is not installed.");
    }

    #[test]
    fn fill_placeholders() {
        assert_eq!(fill("{} and {}", &[&1, &"b"]), "1 and b");
        assert_eq!(fill("no args", &[]), "no args");
        assert_eq!(fill("{} {}", &[&"only"]), "only ");
    }

    #[test]
    fn catalog_is_consistent() {
        let mut seen = BTreeSet::new();
        for (en, ru) in RU {
            assert!(seen.insert(*en), "duplicate msgid {en:?}");
            assert_eq!(
                en.matches("{}").count(),
                ru.matches("{}").count(),
                "placeholder count differs: {en:?}"
            );
            assert!(!ru.is_empty());
        }
    }

    /// Every `t!("...")` literal in the sources must have a Russian entry.
    #[test]
    fn every_message_is_translated() {
        let sources = [
            include_str!("app.rs"),
            include_str!("main.rs"),
            include_str!("plan.rs"),
            include_str!("prefix.rs"),
            include_str!("runner.rs"),
            include_str!("integrate.rs"),
            include_str!("desktop.rs"),
        ];
        let keys: BTreeSet<&str> = RU.iter().map(|(en, _)| *en).collect();
        let mut missing = Vec::new();
        let mut found = 0;
        for src in sources {
            let code = src.split("#[cfg(test)]").next().unwrap();
            for (i, _) in code.match_indices("t!(\"") {
                // Not the tail of `format!(`, `assert!(`, ...
                let before = code[..i].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let lit = rust_string_literal(&code[i + 3..]);
                found += 1;
                if !keys.contains(lit.as_str()) {
                    missing.push(lit);
                }
            }
        }
        assert!(found > 50, "scanner found only {found} messages");
        assert!(missing.is_empty(), "untranslated: {missing:#?}");
    }

    /// Value of the Rust string literal at the start of `s` (escapes and
    /// line continuations handled).
    fn rust_string_literal(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s[1..].chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('\'') => out.push('\''),
                    Some('\n') => {
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                    }
                    Some(o) => {
                        out.push('\\');
                        out.push(o);
                    }
                    None => break,
                },
                c => out.push(c),
            }
        }
        out
    }
}
