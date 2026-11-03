//! The standard library of each language, to tell `$std` modules apart.

use crate::lang::registry;

/// The pattern in `may_use_external` and `forbid_external` that stands for
/// the standard library of the importing file's language.
pub const STD: &str = "$std";

/// Whether `module`, imported by `file`, belongs to the standard library of
/// the file's language. Languages without a rule here never match.
pub fn is_std(file: &str, module: &str) -> bool {
    registry::of(file)
        .and_then(|language| language.std)
        .is_some_and(|std| std(module))
}

/// Whether `$std` can tell the standard library of `file`'s language.
pub fn knows(file: &str) -> bool {
    // What a manifest declares is never the standard library.
    registry::manifest_of(file).is_some() || registry::of(file).is_some_and(|l| l.std.is_some())
}

/// The first part of a module path, up to any of `seps`.
fn first<'a>(module: &'a str, seps: &[char]) -> &'a str {
    module.split(seps).next().unwrap_or(module)
}

/// A standard package's path has no domain: `fmt`, `net/http`; `C` is cgo.
pub fn go(module: &str) -> bool {
    module != "C" && !first(module, &['/']).contains('.')
}

pub fn python(module: &str) -> bool {
    PYTHON.binary_search(&first(module, &['.'])).is_ok()
}

pub fn java(module: &str) -> bool {
    jdk(module)
}

pub fn kotlin(module: &str) -> bool {
    first(module, &['.']) == "kotlin" || jdk(module)
}

pub fn rust(module: &str) -> bool {
    ["std", "proc_macro"].contains(&first(module, &[':']))
}

pub fn dart(module: &str) -> bool {
    module.starts_with("dart:")
}

pub fn csharp(module: &str) -> bool {
    first(module, &['.']) == "System"
}

/// A class of no namespace is PHP's own: `DateTime`, `PDO`, `Exception`.
pub fn php(module: &str) -> bool {
    !module.contains('\\')
}

pub fn swift(module: &str) -> bool {
    SWIFT.contains(&first(module, &['.']))
}

/// The modules of the Swift toolchain and Apple's SDKs that a Swift file imports.
const SWIFT: &[&str] = &[
    "AVFoundation",
    "Accelerate",
    "AppKit",
    "Combine",
    "Contacts",
    "CoreBluetooth",
    "CoreData",
    "CoreFoundation",
    "CoreGraphics",
    "CoreImage",
    "CoreLocation",
    "CoreML",
    "CoreMotion",
    "CoreText",
    "CryptoKit",
    "Darwin",
    "Dispatch",
    "Foundation",
    "FoundationNetworking",
    "Glibc",
    "MapKit",
    "Metal",
    "Network",
    "Observation",
    "OSLog",
    "Photos",
    "QuartzCore",
    "RegexBuilder",
    "SafariServices",
    "SceneKit",
    "Security",
    "SpriteKit",
    "StoreKit",
    "Swift",
    "SwiftData",
    "SwiftUI",
    "Synchronization",
    "SystemConfiguration",
    "Testing",
    "UIKit",
    "UniformTypeIdentifiers",
    "UserNotifications",
    "WebKit",
    "WidgetKit",
    "XCTest",
    "os",
    "simd",
];

/// The packages of the JDK: `java.util`, `javax.crypto`, `org.w3c.dom`.
fn jdk(module: &str) -> bool {
    [
        "java.",
        "javax.",
        "jdk.",
        "sun.",
        "com.sun.",
        "org.w3c.",
        "org.xml.sax",
        "org.ietf.jgss",
    ]
    .iter()
    .any(|prefix| module.starts_with(prefix))
}

/// The standard library of Python: `sys.stdlib_module_names` of Python
/// 3.15, private modules included, and the modules removed since 3.8
/// (`distutils`, `imp`, `cgi`, …), which older projects still import.
/// Regenerate with `python3 -c "import sys; print(sorted(sys.stdlib_module_names))"`
/// and keep the removed ones.
const PYTHON: &[&str] = &[
    "__future__",
    "_abc",
    "_aix_support",
    "_android_support",
    "_apple_support",
    "_ast",
    "_ast_unparse",
    "_asyncio",
    "_bisect",
    "_blake2",
    "_bz2",
    "_codecs",
    "_codecs_cn",
    "_codecs_hk",
    "_codecs_iso2022",
    "_codecs_jp",
    "_codecs_kr",
    "_codecs_tw",
    "_collections",
    "_collections_abc",
    "_colorize",
    "_compat_pickle",
    "_contextvars",
    "_csv",
    "_ctypes",
    "_curses",
    "_curses_panel",
    "_datetime",
    "_dbm",
    "_decimal",
    "_dummy_thread",
    "_elementtree",
    "_frozen_importlib",
    "_frozen_importlib_external",
    "_functools",
    "_gdbm",
    "_hashlib",
    "_heapq",
    "_hmac",
    "_imp",
    "_interpchannels",
    "_interpqueues",
    "_interpreters",
    "_io",
    "_ios_support",
    "_json",
    "_locale",
    "_lsprof",
    "_lzma",
    "_markupbase",
    "_math_integer",
    "_md5",
    "_multibytecodec",
    "_multiprocessing",
    "_opcode",
    "_opcode_metadata",
    "_operator",
    "_osx_support",
    "_overlapped",
    "_pickle",
    "_posixshmem",
    "_posixsubprocess",
    "_py_abc",
    "_py_warnings",
    "_pydatetime",
    "_pydecimal",
    "_pyio",
    "_pylong",
    "_pyrepl",
    "_queue",
    "_random",
    "_remote_debugging",
    "_scproxy",
    "_sha1",
    "_sha2",
    "_sha3",
    "_signal",
    "_sitebuiltins",
    "_socket",
    "_sqlite3",
    "_sre",
    "_ssl",
    "_stat",
    "_statistics",
    "_string",
    "_strptime",
    "_struct",
    "_suggestions",
    "_symtable",
    "_sysconfig",
    "_thread",
    "_threading_local",
    "_tkinter",
    "_tokenize",
    "_tracemalloc",
    "_types",
    "_typing",
    "_uuid",
    "_warnings",
    "_weakref",
    "_weakrefset",
    "_winapi",
    "_wmi",
    "_zoneinfo",
    "_zstd",
    "abc",
    "aifc",
    "annotationlib",
    "antigravity",
    "argparse",
    "array",
    "ast",
    "asynchat",
    "asyncio",
    "asyncore",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "binascii",
    "binhex",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cgi",
    "cgitb",
    "chunk",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "compression",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "crypt",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "distutils",
    "doctest",
    "dummy_threading",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "formatter",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "genericpath",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "imghdr",
    "imp",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "lib2to3",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "macpath",
    "mailbox",
    "mailcap",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "msilib",
    "msvcrt",
    "multiprocessing",
    "netrc",
    "nis",
    "nntplib",
    "nt",
    "ntpath",
    "nturl2path",
    "numbers",
    "opcode",
    "operator",
    "optparse",
    "os",
    "ossaudiodev",
    "parser",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pipes",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "profiling",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "pydoc_data",
    "pyexpat",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtpd",
    "smtplib",
    "sndhdr",
    "socket",
    "socketserver",
    "spwd",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "sunau",
    "symbol",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_standard_library_by_language() {
        assert!(is_std("a.go", "net/http"));
        assert!(!is_std("a.go", "github.com/lib/pq"));
        assert!(is_std("a.py", "os.path"));
        assert!(is_std("a.py", "collections"));
        assert!(!is_std("a.py", "requests"));
        assert!(is_std("A.java", "java.util.List"));
        assert!(!is_std("A.java", "com.google.common.Lists"));
        assert!(is_std("A.kt", "kotlin.math.max"));
        assert!(is_std("a.rs", "std::collections"));
        assert!(!is_std("a.rs", "serde::Deserialize"));
        assert!(is_std("A.cs", "System.Text.Json"));
        assert!(!is_std("a.ts", "fs"));
        assert!(!is_std("a.go", "C"));
        assert!(is_std("a.py", "distutils.core"));
        assert!(is_std("a.py", "_thread"));
        assert!(is_std("A.java", "org.w3c.dom.Node"));
        assert!(!is_std("A.java", "org.apache.commons.Lang"));
        assert!(knows("a.go") && !knows("a.ts"));
        assert!(is_std("V.swift", "Foundation") && !is_std("V.swift", "Alamofire"));
        assert!(is_std("a.php", "DateTime") && !is_std("a.php", "Illuminate\\Support\\Str"));
    }

    #[test]
    fn the_python_list_is_sorted_for_binary_search() {
        assert!(PYTHON.windows(2).all(|w| w[0] < w[1]));
    }
}
