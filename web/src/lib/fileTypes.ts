/**
 * Which kind of file something is, for its icon and its type description: formats, programming languages and the
 * files of common engineering tools, recognised by exact file name, then name pattern, then extension, then MIME type.
 *
 * Only for how a file looks. What can be done with it (preview, text editing, thumbnails) is decided by `categoryOf`
 * and the checks next to it (components/FileIcon.tsx), which this never changes.
 *
 * Marks are either a symbol of the icon set the interface already uses (lucide, ISC licence) or a label drawn on a
 * page (components/FileIcon.tsx): no third-party logos are bundled.
 */
import {
  AppWindowIcon,
  AtomIcon,
  Axis3dIcon,
  BinaryIcon,
  BirdIcon,
  BookOpenIcon,
  BookTextIcon,
  BracesIcon,
  CalendarIcon,
  CaptionsIcon,
  ChartScatterIcon,
  CodeXmlIcon,
  CoffeeIcon,
  ContactIcon,
  ContainerIcon,
  DatabaseIcon,
  Disc3Icon,
  DraftingCompassIcon,
  FileBadgeIcon,
  FileClockIcon,
  FileCogIcon,
  FileDiffIcon,
  FileKeyIcon,
  FileLockIcon,
  FileTextIcon,
  GemIcon,
  GitBranchIcon,
  HammerIcon,
  HardDriveIcon,
  KeyRoundIcon,
  LinkIcon,
  MagnetIcon,
  MailIcon,
  NotebookPenIcon,
  PackageIcon,
  PaletteIcon,
  PenToolIcon,
  RocketIcon,
  ScaleIcon,
  ScrollTextIcon,
  SigmaIcon,
  SquareTerminalIcon,
  TypeIcon,
  WorkflowIcon,
  type LucideIcon,
} from "lucide-react";
import { t } from "@/lib/i18n";

/** A symbol in a colour, or a short label on a page */
export type Mark = { icon: LucideIcon; color: string } | { label: string; bg: string };

export interface FileType {
  /** Stable id, e.g. "rust", "json", "dockerfile" */
  id: string;
  /** Accessible description, e.g. "Rust source file" */
  title: string;
  mark: Mark;
}

/** What a file is known by: its name and MIME type, and its size when known (some extensions need it) */
export interface FileLike {
  name: string;
  mime: string;
  size?: number;
}

// Descriptions: language and format names aren't translated, so a few sentences cover them all
const source = (name: string) => t("{name} source file", { name });
const file = (name: string) => t("{name} file", { name });

const label = (id: string, text: string, bg: string, title: string): FileType => ({ id, title, mark: { label: text, bg } });
const symbol = (id: string, icon: LucideIcon, color: string, title: string): FileType => ({ id, title, mark: { icon, color } });

// ───────────── The types ─────────────

// Programming languages
const LANG = {
  rust: label("rust", "RS", "#b7410e", source("Rust")),
  go: label("go", "GO", "#00a7d0", source("Go")),
  js: label("js", "JS", "#e8d44d", source("JavaScript")),
  ts: label("ts", "TS", "#3178c6", source("TypeScript")),
  react: symbol("react", AtomIcon, "text-[#0e9fc4] dark:text-[#61dafb]", source("React (JSX)")),
  python: label("python", "PY", "#3572a5", source("Python")),
  pythonBin: label("python-compiled", "PYC", "#5a7ea3", t("Compiled Python file")),
  java: symbol("java", CoffeeIcon, "text-[#b07219] dark:text-[#e0a050]", source("Java")),
  javaBin: symbol("java-archive", CoffeeIcon, "text-[#8a6d4b] dark:text-[#c8a27a]", t("Java archive or class file")),
  c: label("c", "C", "#555599", source("C")),
  cpp: label("cpp", "C++", "#f34b7d", source("C++")),
  csharp: label("csharp", "C#", "#178600", source("C#")),
  php: label("php", "PHP", "#777bb4", source("PHP")),
  ruby: symbol("ruby", GemIcon, "text-[#cc342d] dark:text-[#e5625c]", source("Ruby")),
  kotlin: label("kotlin", "KT", "#7f52ff", source("Kotlin")),
  swift: symbol("swift", BirdIcon, "text-[#f05138]", source("Swift")),
  vue: label("vue", "VUE", "#41b883", file("Vue")),
  svelte: label("svelte", "SV", "#ff3e00", file("Svelte")),
  astro: symbol("astro", RocketIcon, "text-[#ff5d01]", file("Astro")),
  html: symbol("html", CodeXmlIcon, "text-[#e34c26] dark:text-[#f0714f]", file("HTML")),
  css: label("css", "CSS", "#563d7c", file("CSS")),
  sass: label("sass", "SCSS", "#c6538c", file("Sass")),
  less: label("less", "LESS", "#1d365d", file("Less")),
  stylus: label("stylus", "STYL", "#ff6347", file("Stylus")),
  shell: symbol("shell", SquareTerminalIcon, "text-[#3f9b2f] dark:text-[#89e051]", t("Shell script")),
  powershell: label("powershell", "PS", "#012456", t("PowerShell script")),
  batch: label("batch", "BAT", "#4d4d4d", t("Windows batch file")),
  r: label("r", "R", "#198ce7", source("R")),
  julia: label("julia", "JL", "#9558b2", source("Julia")),
  dart: label("dart", "DART", "#00b4ab", source("Dart")),
  lua: label("lua", "LUA", "#000080", source("Lua")),
  perl: label("perl", "PL", "#0298c3", source("Perl")),
  raku: label("raku", "RAKU", "#0000fb", source("Raku")),
  scala: label("scala", "SC", "#c22d40", source("Scala")),
  elixir: label("elixir", "EX", "#6e4a7e", source("Elixir")),
  erlang: label("erlang", "ERL", "#b83998", source("Erlang")),
  haskell: label("haskell", "HS", "#5e5086", source("Haskell")),
  clojure: label("clojure", "CLJ", "#5881d8", source("Clojure")),
  fsharp: label("fsharp", "F#", "#b845fc", source("F#")),
  vb: label("vb", "VB", "#945db7", source("Visual Basic")),
  zig: label("zig", "ZIG", "#ec915c", source("Zig")),
  nim: label("nim", "NIM", "#b39a00", source("Nim")),
  cuda: label("cuda", "CU", "#3a8a00", source("CUDA")),
  asm: label("asm", "ASM", "#6e4c13", source("Assembly")),
  hdl: label("hdl", "HDL", "#848bf3", source("Verilog / VHDL")),
  objc: label("objc", "OBJC", "#438eff", source("Objective-C")),
  matlab: label("matlab", "M", "#e16737", source("MATLAB / Objective-C")),
  groovy: label("groovy", "GVY", "#4298b8", source("Groovy")),
  pascal: label("pascal", "PAS", "#e3f171", source("Pascal")),
  tcl: label("tcl", "TCL", "#e4cc98", source("Tcl")),
} satisfies Record<string, FileType>;

// Data, markup and definitions
const DATA = {
  json: symbol("json", BracesIcon, "text-[#b8a200] dark:text-[#e0cf5a]", file("JSON")),
  xml: label("xml", "XML", "#0060ac", file("XML")),
  yaml: label("yaml", "YML", "#cb171e", file("YAML")),
  toml: label("toml", "TOML", "#9c4221", file("TOML")),
  sql: label("sql", "SQL", "#e38c00", file("SQL")),
  graphql: label("graphql", "GQL", "#e10098", file("GraphQL")),
  proto: label("proto", "PB", "#4a78c2", file("Protocol Buffers")),
  terraform: label("terraform", "TF", "#7b42bc", file("Terraform")),
  nix: label("nix", "NIX", "#5277c3", file("Nix")),
  bicep: label("bicep", "BCP", "#519aba", file("Bicep")),
  jupyter: symbol("jupyter", NotebookPenIcon, "text-[#f37626]", t("Jupyter notebook")),
  latex: symbol("latex", SigmaIcon, "text-[#3d6117] dark:text-[#8fbf5a]", t("LaTeX document")),
  rst: label("rst", "RST", "#141414", file("reStructuredText")),
  asciidoc: label("asciidoc", "ADOC", "#e40046", file("AsciiDoc")),
  org: label("org", "ORG", "#77aa99", file("Org")),
  patch: symbol("patch", FileDiffIcon, "text-[#d24a3f] dark:text-[#ef7a70]", t("Patch or diff")),
  http: label("http", "HTTP", "#005c9c", t("HTTP requests")),
  wasm: label("wasm", "WASM", "#654ff0", t("WebAssembly module")),
  sourcemap: label("sourcemap", "MAP", "#6d8086", t("Source map")),
  env: symbol("env", KeyRoundIcon, "text-[#c99a06] dark:text-[#ecd53f]", t("Environment settings")),
  config: symbol("config", FileCogIcon, "text-[#6d8086] dark:text-[#9fb0ba]", t("Configuration file")),
  data: symbol("data", ChartScatterIcon, "text-[#0f8f8f] dark:text-[#4fc9c9]", t("Data file")),
  log: symbol("log", ScrollTextIcon, "text-muted-foreground", t("Log file")),
  subtitles: symbol("subtitles", CaptionsIcon, "text-[#7b69da] dark:text-[#a99aee]", t("Subtitles")),
} satisfies Record<string, FileType>;

// Tools and projects
const TOOL = {
  docker: symbol("docker", ContainerIcon, "text-[#1d63ed] dark:text-[#4c8dff]", t("Container build file")),
  compose: symbol("compose", ContainerIcon, "text-[#0a9396] dark:text-[#4cc9c9]", t("Container compose file")),
  build: symbol("build", HammerIcon, "text-[#6d8086] dark:text-[#9fb0ba]", t("Build file")),
  ci: symbol("ci", WorkflowIcon, "text-[#d24939] dark:text-[#ef7a70]", t("Pipeline file")),
  manifest: symbol("manifest", PackageIcon, "text-[#a0522d] dark:text-[#d99a6c]", t("Package or project file")),
  lock: symbol("lock", FileLockIcon, "text-[#8b8b8b]", t("Lock file")),
  git: symbol("git", GitBranchIcon, "text-[#f05032]", t("Git settings")),
  license: symbol("license", ScaleIcon, "text-[#b8860b] dark:text-[#e0b84a]", t("License")),
  readme: symbol("readme", BookTextIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Project documentation")),
} satisfies Record<string, FileType>;

// Other formats
const FORMAT = {
  wordTemplate: symbol("word-template", FileTextIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Document template")),
  word: symbol("word-other", FileTextIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Document")),
  sheetTemplate: label("sheet-template", "XLT", "#217346", t("Spreadsheet template or add-in")),
  sheet: label("sheet-other", "XLS", "#35a26c", t("Spreadsheet")),
  slidesTemplate: label("slides-template", "POT", "#c43e1c", t("Presentation template or show")),
  slides: label("slides-other", "PPT", "#dc7a3c", t("Presentation")),
  publisher: label("publisher", "PUB", "#077568", t("Publication")),
  photoshop: label("photoshop", "PSD", "#0c77c2", t("Photoshop image")),
  illustrator: label("illustrator", "AI", "#c86b00", t("Illustrator drawing")),
  indesign: label("indesign", "INDD", "#d4145a", t("InDesign document")),
  vector: symbol("vector", PenToolIcon, "text-[#c86b00] dark:text-[#f0a040]", t("Vector drawing")),
  design: symbol("design", PaletteIcon, "text-[#a259ff]", t("Design file")),
  ebook: symbol("ebook", BookOpenIcon, "text-[#5b8c2a] dark:text-[#9ccc65]", t("E-book")),
  installer: symbol("installer", AppWindowIcon, "text-[#0078d4] dark:text-[#4ca8f0]", t("Program or installer")),
  package: symbol("package", PackageIcon, "text-[#a0522d] dark:text-[#d99a6c]", t("Software package")),
  binary: symbol("binary", BinaryIcon, "text-[#6d8086] dark:text-[#9fb0ba]", t("Compiled binary")),
  disc: symbol("disc", Disc3Icon, "text-[#6d8086] dark:text-[#9fb0ba]", t("Disk image")),
  vm: symbol("vm-disk", HardDriveIcon, "text-[#6d8086] dark:text-[#9fb0ba]", t("Virtual disk")),
  font: symbol("font", TypeIcon, "text-[#c2185b] dark:text-[#f06292]", t("Font")),
  database: symbol("database", DatabaseIcon, "text-[#4f6d7a] dark:text-[#90a4ae]", t("Database")),
  cad: symbol("cad", DraftingCompassIcon, "text-[#c62828] dark:text-[#ef7a70]", t("CAD drawing")),
  model: symbol("model", Axis3dIcon, "text-[#e8710a] dark:text-[#ffa24d]", t("3D model")),
  certificate: symbol("certificate", FileBadgeIcon, "text-[#0f8a7e] dark:text-[#4fc9b8]", t("Certificate")),
  signature: symbol("signature", FileKeyIcon, "text-[#0f8a7e] dark:text-[#4fc9b8]", t("Signature or key")),
  torrent: symbol("torrent", MagnetIcon, "text-[#c62828] dark:text-[#ef7a70]", t("Torrent")),
  shortcut: symbol("shortcut", LinkIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Shortcut")),
  backup: symbol("backup", FileClockIcon, "text-muted-foreground", t("Backup or temporary file")),
  calendar: symbol("calendar", CalendarIcon, "text-[#d93025] dark:text-[#f28b82]", t("Calendar")),
  contact: symbol("contact", ContactIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Contact card")),
  email: symbol("email", MailIcon, "text-[#3f86e0] dark:text-[#77acf2]", t("Email message")),
} satisfies Record<string, FileType>;

export const FILE_TYPES = { ...LANG, ...DATA, ...TOOL, ...FORMAT };

// ───────────── Rules ─────────────

/** Exact file names (lowercase), which win over everything else */
const NAMES: Record<string, FileType> = {};
const names = (type: FileType, ...list: string[]) => list.forEach((n) => (NAMES[n] = type));
names(TOOL.docker, "dockerfile", "containerfile");
names(TOOL.compose, "compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml");
names(TOOL.build, "makefile", "gnumakefile", "rakefile", "justfile", "build", "build.bazel", "workspace", "workspace.bazel", "cmakelists.txt", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "meson.build", "vagrantfile", "procfile", "taskfile.yml");
names(TOOL.ci, "jenkinsfile", ".gitlab-ci.yml", ".travis.yml", "azure-pipelines.yml", "bitbucket-pipelines.yml", "appveyor.yml");
names(
  TOOL.manifest,
  "package.json",
  "cargo.toml",
  "pyproject.toml",
  "setup.py",
  "setup.cfg",
  "go.mod",
  "go.work",
  "gemfile",
  "pipfile",
  "composer.json",
  "pom.xml",
  "deno.json",
  "pubspec.yaml",
  "mix.exs",
  "project.clj",
  "requirements.txt",
  "environment.yml",
);
names(
  TOOL.lock,
  "package-lock.json",
  "pnpm-lock.yaml",
  "yarn.lock",
  "cargo.lock",
  "poetry.lock",
  "uv.lock",
  "pipfile.lock",
  "composer.lock",
  "gemfile.lock",
  "bun.lock",
  "bun.lockb",
  "go.sum",
  "go.work.sum",
  "flake.lock",
  "mix.lock",
  "pubspec.lock",
  "deno.lock",
);
names(TOOL.git, ".gitignore", ".gitattributes", ".gitmodules", ".gitconfig", ".gitkeep", ".mailmap", ".git-blame-ignore-revs");
names(TOOL.license, "license", "licence", "copying", "notice", "unlicense", "license.txt", "licence.txt", "copying.txt", "notice.txt");
names(TOOL.readme, "readme", "authors", "changelog", "changes", "contributing", "contributors", "history", "todo", "readme.txt", "authors.txt", "changelog.txt");
names(
  DATA.config,
  ".editorconfig",
  ".npmrc",
  ".yarnrc",
  ".yarnrc.yml",
  ".nvmrc",
  ".node-version",
  ".python-version",
  ".ruby-version",
  ".tool-versions",
  ".prettierrc",
  ".eslintrc",
  ".babelrc",
  ".browserslistrc",
  ".stylelintrc",
  ".dockerignore",
  ".prettierignore",
  ".eslintignore",
  ".npmignore",
  ".htaccess",
  "tsconfig.json",
  "jsconfig.json",
);

/** File name patterns, tried in order after the exact names */
const PATTERNS: [RegExp, FileType][] = [
  [/^(dockerfile|containerfile)[.-]|\.(dockerfile|containerfile)$/, TOOL.docker],
  [/^(docker-)?compose[.-].*\.ya?ml$/, TOOL.compose],
  [/^\.env($|\.)|\.env$/, DATA.env],
  [/^\.github\/workflows\/|^\.?(github|gitlab)-ci\b/, TOOL.ci],
  [/^\.(prettier|eslint|stylelint|babel|swc|lintstaged|commitlint|markdownlint)rc\b|^(prettier|eslint|stylelint|babel|vite|vitest|webpack|rollup|jest|tailwind|postcss|next|nuxt|svelte|astro)\.config\.[cm]?[jt]s$/, DATA.config],
  [/^tsconfig\..*\.json$/, DATA.config],
  [/^id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$|^(authorized_keys|known_hosts)$/, FORMAT.signature],
  [/^requirements[-_.].*\.(txt|in)$/, TOOL.manifest],
  [/^(license|licence|copying)[-_.]/, TOOL.license],
  [/^(readme|changelog|authors|contributing)\.(txt|rst|adoc)$/, TOOL.readme],
];

/** Extensions of more than one part, tried before the last part alone */
const COMPOUND: [string, FileType | "archive"][] = [
  ["d.ts", LANG.ts],
  ["d.mts", LANG.ts],
  ["d.cts", LANG.ts],
  ["js.map", DATA.sourcemap],
  ["css.map", DATA.sourcemap],
  ["tar.gz", "archive"],
  ["tar.bz2", "archive"],
  ["tar.xz", "archive"],
  ["tar.zst", "archive"],
  ["tar.lz", "archive"],
  ["tar.lz4", "archive"],
];

/** Extensions (lowercase) */
const EXT: Record<string, FileType | "archive"> = {};
const exts = (type: FileType | "archive", list: string) => list.split(" ").forEach((e) => (EXT[e] = type));
exts(LANG.rust, "rs");
exts(LANG.go, "go");
exts(LANG.js, "js cjs mjs");
exts(LANG.ts, "ts cts mts");
exts(LANG.react, "jsx tsx");
exts(LANG.python, "py pyw pyi");
exts(LANG.pythonBin, "pyc pyd pyo whl wheel");
exts(LANG.java, "java");
exts(LANG.javaBin, "class jar war ear");
exts(LANG.c, "c h");
exts(LANG.cpp, "cpp cc cxx c++ hpp hh hxx h++ ino");
exts(LANG.csharp, "cs csx");
exts(LANG.php, "php phtml");
exts(LANG.ruby, "rb erb gemspec rake");
exts(LANG.kotlin, "kt kts");
exts(LANG.swift, "swift");
exts(LANG.vue, "vue");
exts(LANG.svelte, "svelte");
exts(LANG.astro, "astro");
exts(LANG.html, "html htm xhtml");
exts(LANG.css, "css");
exts(LANG.sass, "sass scss");
exts(LANG.less, "less");
exts(LANG.stylus, "styl");
exts(LANG.shell, "sh bash zsh fish ksh csh");
exts(LANG.powershell, "ps1 psm1 psd1");
exts(LANG.batch, "bat cmd");
exts(LANG.r, "r rmd");
exts(LANG.julia, "jl");
exts(LANG.dart, "dart");
exts(LANG.lua, "lua luac");
exts(LANG.perl, "pl pm");
exts(LANG.raku, "raku rakumod p6");
exts(LANG.scala, "scala sc sbt");
exts(LANG.elixir, "ex exs");
exts(LANG.erlang, "erl hrl");
exts(LANG.haskell, "hs lhs");
exts(LANG.clojure, "clj cljs cljc edn");
exts(LANG.fsharp, "fs fsx fsi");
exts(LANG.vb, "vb vbs vbe");
exts(LANG.zig, "zig");
exts(LANG.nim, "nim");
exts(LANG.cuda, "cu cuh");
exts(LANG.asm, "s asm");
exts(LANG.hdl, "v sv svh vhdl");
exts(LANG.matlab, "m");
exts(LANG.objc, "mm");
exts(LANG.groovy, "groovy gvy");
exts(LANG.pascal, "pas pp");
exts(LANG.tcl, "tcl tk");
exts(DATA.json, "json jsonc json5 jsonl ndjson geojson webmanifest");
exts(DATA.xml, "xml xsd xsl xslt plist resx");
exts(DATA.yaml, "yaml yml");
exts(DATA.toml, "toml");
exts(DATA.sql, "sql psql");
exts(DATA.graphql, "graphql gql");
exts(DATA.proto, "proto");
exts(DATA.terraform, "tf tfvars hcl nomad");
exts(DATA.nix, "nix");
exts(DATA.bicep, "bicep");
exts(DATA.jupyter, "ipynb");
exts(DATA.latex, "tex latex sty cls bib");
exts(DATA.rst, "rst");
exts(DATA.asciidoc, "adoc asciidoc");
exts(DATA.org, "org");
exts(DATA.patch, "patch diff");
exts(DATA.http, "http rest");
exts(DATA.wasm, "wasm");
exts(DATA.sourcemap, "map");
exts(DATA.env, "env");
exts(DATA.config, "ini conf cfg properties config reg service timer socket desktop editorconfig");
exts(DATA.data, "parquet arrow feather h5 hdf5 nc npy npz pickle pkl mat rds rdata avro orc");
exts(DATA.log, "log");
exts(DATA.subtitles, "srt vtt ass ssa sub");
exts(TOOL.build, "gradle cmake mk pro pri props targets sln csproj vbproj vcxproj fsproj pbxproj xcodeproj xcworkspace");
exts(TOOL.lock, "lock");
exts(FORMAT.wordTemplate, "dot dotx dotm ott");
exts(FORMAT.word, "docm pages wps wpd abw lwp");
exts(FORMAT.sheetTemplate, "xlt xltx xltm xla xlam ots xll");
exts(FORMAT.sheet, "numbers gnumeric");
exts(FORMAT.slidesTemplate, "pot potx potm pps ppsx ppsm otp ppa ppam");
exts(FORMAT.publisher, "pub");
exts(FORMAT.photoshop, "psd psb");
exts(FORMAT.illustrator, "ai");
exts(FORMAT.indesign, "indd idml");
exts(FORMAT.vector, "eps ps cdr");
exts(FORMAT.design, "xcf sketch fig afdesign afphoto afpub kra");
exts(FORMAT.ebook, "epub mobi azw azw3 fb2 lit djvu");
exts(FORMAT.installer, "exe msi msix appx appimage app");
exts(FORMAT.package, "deb rpm apk aab ipa nupkg snap flatpak pkg crx xpi vsix");
exts(FORMAT.binary, "dll so dylib lib a o pdb elf bin dat");
exts(FORMAT.disc, "iso img dmg toast cue");
exts(FORMAT.vm, "vhdx qcow2 vmdk vdi ova ovf");
exts(FORMAT.font, "ttf otf woff woff2 eot ttc fon pfb");
exts(FORMAT.database, "db sqlite sqlite3 db3 mdb accdb dbf frm ibd");
exts(FORMAT.cad, "dwg dxf dwt dgn");
exts(FORMAT.model, "obj stl fbx blend glb gltf step stp iges igs 3mf skp 3ds dae ply usdz usd");
exts(FORMAT.certificate, "pem crt cer der pfx p12 p7b p7c csr crl");
exts(FORMAT.signature, "asc gpg pgp sig p7s p7m");
exts(FORMAT.torrent, "torrent");
exts(FORMAT.shortcut, "url lnk webloc");
exts(FORMAT.backup, "bak tmp temp old orig swp");
exts(FORMAT.calendar, "ics");
exts(FORMAT.contact, "vcf");
exts(FORMAT.email, "eml msg mbox");
exts("archive", "bz tbz tbz2 txz z zst lz lzma lz4 br cab tgz arj lzh ace cpio shar sit sitx");

/** Extensions that are also a video format (MPEG transport streams): source files are small, videos aren't */
const SCRIPT_OR_VIDEO = /^(ts|mts)$/;
/** The largest `.ts` / `.mts` file taken for source code rather than a video, when the size is known */
export const MAX_SCRIPT_BYTES = 1024 * 1024;

/**
 * Whether a `.ts` or `.mts` file is TypeScript rather than a video: the MIME type alone can't tell (both have the
 * video one), so a small file is taken for source code; without a size, only a MIME type that isn't video counts
 */
export function isScriptNotVideo(f: FileLike): boolean {
  const ext = extension(f.name);
  if (!SCRIPT_OR_VIDEO.test(ext)) return false;
  if (!f.mime.toLowerCase().startsWith("video/")) return true;
  return f.size !== undefined && f.size <= MAX_SCRIPT_BYTES;
}

const extension = (name: string) => {
  const base = name.slice(name.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
};

/**
 * The specific kind of a file, or "archive" (shown with the archive icon), or null when only its general category is
 * known (the category's icon then shows). Names are matched without regard to letter case; hidden files (".env")
 * and names without an extension ("Dockerfile") are recognised by their whole name.
 */
export function fileTypeOf(f: FileLike): FileType | "archive" | null {
  const path = f.name.toLowerCase();
  const name = path.slice(path.lastIndexOf("/") + 1);
  const exact = NAMES[name];
  if (exact) return exact;
  for (const [re, type] of PATTERNS) if (re.test(name) || re.test(path)) return type;
  for (const [ext, type] of COMPOUND) if (name.endsWith(`.${ext}`) && name.length > ext.length + 1) return type;
  const ext = extension(name);
  if (SCRIPT_OR_VIDEO.test(ext)) return isScriptNotVideo(f) ? LANG.ts : null;
  // VHDL source or a Hyper-V virtual disk: disks are large
  if (ext === "vhd") return f.size !== undefined && f.size <= MAX_SCRIPT_BYTES ? LANG.hdl : FORMAT.vm;
  return EXT[ext] ?? null;
}
