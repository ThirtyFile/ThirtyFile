// File icons by format, language and tool (lib/fileTypes.ts), and that they leave what can be done with a file alone
// (categoryOf and the checks in components/FileIcon.tsx)
import { describe, expect, test } from "vitest";
import type { Node } from "@/api";
import { categoryOf, isBrowserMedia, isTextLike, typeTitle } from "@/components/FileIcon";
import { MAX_SCRIPT_BYTES, fileTypeOf } from "@/lib/fileTypes";

/** The MIME type the server gives a name (mime_guess), for the names these tests need */
const SERVER_MIME: Record<string, string> = {
  json: "application/json",
  xml: "text/xml",
  ts: "video/vnd.dlna.mpeg-tts",
  mts: "video/vnd.dlna.mpeg-tts",
  mod: "video/mpeg",
  rpm: "audio/x-pn-realaudio-plugin",
  dotx: "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
  xltx: "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
  potx: "application/vnd.openxmlformats-officedocument.presentationml.template",
  ppsx: "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
  js: "application/javascript",
  mp4: "video/mp4",
};

function node(name: string, size = 1000, mime?: string): Node {
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
  return {
    id: name,
    parent_id: null,
    kind: "file",
    name,
    size,
    mime: mime ?? SERVER_MIME[ext] ?? "application/octet-stream",
    created_at: 0,
    updated_at: 0,
    trashed_at: null,
    drive_id: null,
    owner_name: "",
    is_favorite: false,
  };
}

const id = (name: string, size?: number, mime?: string) => {
  const t = fileTypeOf(node(name, size, mime));
  return t === null ? null : t === "archive" ? "archive" : t.id;
};

describe("icons", () => {
  test("JSON, Rust and Go are three different icons, side by side in one folder", () => {
    const ids = ["config.json", "main.rs", "main.go"].map((n) => id(n));
    expect(ids).toEqual(["json", "rust", "go"]);
    expect(typeTitle(node("main.rs"))).toBe("Rust source file");
  });

  test("the reviewed languages and data formats each have their own icon", () => {
    // One name per language or format of the inventory in #208; aliases of one format share its icon
    const groups: string[][] = [
      ["json"], ["xml"], ["yaml", "yml"], ["toml"], ["sql"], ["rs"], ["go"], ["js", "cjs", "mjs"], ["ts", "cts"], ["jsx", "tsx"],
      ["py"], ["pyc", "pyd"], ["java"], ["class", "jar", "war", "ear"], ["c", "h"], ["cpp", "cc", "cxx", "hpp", "hxx"], ["cs"], ["php"], ["rb"],
      ["kt", "kts"], ["swift"], ["vue"], ["svelte"], ["astro"], ["html", "htm"], ["css"], ["sass", "scss"], ["less"], ["styl"],
      ["sh", "bash", "zsh", "fish"], ["ps1"], ["bat", "cmd"], ["r", "rmd"], ["jl"], ["dart"], ["lua"], ["pl", "pm"], ["scala", "sc"],
      ["ex", "exs"], ["erl", "hrl"], ["hs", "lhs"], ["clj", "cljs", "cljc", "edn"], ["fs", "fsx", "fsi"], ["vb", "vbs", "vbe"], ["zig"],
      ["nim"], ["cu", "cuh"], ["s", "asm"], ["v", "sv", "vhdl"], ["graphql", "gql"], ["proto"], ["tf", "tfvars", "hcl"], ["nix"],
      ["bicep"], ["ipynb"], ["tex"], ["rst"], ["adoc"], ["patch", "diff"], ["env"], ["ini", "conf", "properties"],
    ];
    const seen = new Map<string, string>();
    for (const group of groups) {
      // Aliases share one icon, which is theirs alone
      const ids = [...new Set(group.map((ext) => id(`file.${ext}`)))];
      expect({ group, ids }).toEqual({ group, ids: [expect.any(String)] });
      expect({ group, sameAs: seen.get(ids[0]!) }).toEqual({ group, sameAs: undefined });
      seen.set(ids[0]!, group.join(" "));
    }
  });

  test("engineering files are recognised by their names, whatever their extension", () => {
    const cases: [string, string][] = [
      ["Dockerfile", "docker"], ["Containerfile", "docker"], ["Dockerfile.production", "docker"], ["api.Dockerfile", "docker"],
      ["compose.yaml", "compose"], ["docker-compose.prod.yml", "compose"],
      ["Makefile", "build"], ["GNUmakefile", "build"], ["Rakefile", "build"], ["Justfile", "build"], ["BUILD", "build"], ["WORKSPACE", "build"],
      ["CMakeLists.txt", "build"], ["Vagrantfile", "build"], ["Procfile", "build"], ["Jenkinsfile", "ci"], [".github/workflows/build.yml", "ci"],
      [".env", "env"], [".env.local", "env"], [".env.example", "env"],
      [".gitignore", "git"], [".gitattributes", "git"], [".gitmodules", "git"],
      [".editorconfig", "config"], [".npmrc", "config"], [".nvmrc", "config"], [".prettierrc", "config"], [".prettierrc.json", "config"],
      [".eslintrc.json", "config"], [".tool-versions", "config"], ["tsconfig.json", "config"], ["tsconfig.app.json", "config"],
      ["package.json", "manifest"], ["Cargo.toml", "manifest"], ["pyproject.toml", "manifest"], ["go.mod", "manifest"], ["Gemfile", "manifest"],
      ["Pipfile", "manifest"], ["requirements.txt", "manifest"], ["requirements-dev.txt", "manifest"],
      ["package-lock.json", "lock"], ["pnpm-lock.yaml", "lock"], ["yarn.lock", "lock"], ["Cargo.lock", "lock"], ["poetry.lock", "lock"],
      ["uv.lock", "lock"], ["Pipfile.lock", "lock"], ["composer.lock", "lock"], ["Gemfile.lock", "lock"], ["bun.lock", "lock"], ["bun.lockb", "lock"],
      ["go.sum", "lock"],
      ["LICENSE", "license"], ["COPYING", "license"], ["NOTICE", "license"], ["LICENSE-MIT", "license"],
      ["README", "readme"], ["AUTHORS", "readme"], ["CHANGELOG", "readme"],
      ["id_ed25519.pub", "signature"],
    ];
    expect(cases.map(([name]) => [name, id(name)])).toEqual(cases);
    // A Markdown README keeps the Markdown icon
    expect(id("README.md")).toBeNull();
  });

  test("the common formats of the inventory no longer fall back to the generic icon", () => {
    const inventory =
      "docm dot dotm ott pages wps pub abw xlt xltm xla xlam ots numbers pot potm pps ppsm otp psd psb ai eps ps xcf sketch fig afdesign afphoto indd " +
      "epub mobi azw azw3 fb2 bz tbz tbz2 txz z zst lz lzma lz4 br cab exe msi dll deb apk aab appimage iso img dmg vhd vhdx qcow2 ova ovf " +
      "ttf otf woff woff2 eot db sqlite sqlite3 mdb accdb dbf parquet arrow feather h5 hdf5 nc npy npz pickle pkl mat rds rdata avro orc " +
      "dwg dxf dwt stl obj fbx blend glb step stp iges igs 3mf skp pem crt cer der pfx p12 p7b p7c p7s asc gpg csr sig amr torrent url lnk bak tmp dat " +
      "r rmd jl pl pm dart m mm kts scala sc ex exs erl hrl hs lhs clj cljs cljc edn fs fsx fsi vbe raku nim zig cu cuh v sv vhd vhdl " +
      "cjs cts astro graphql gql proto http rest bash zsh fish tf tfvars hcl nomad nix bicep service timer socket desktop " +
      "gradle groovy cmake pro pri fsproj props targets xcodeproj pbxproj xcworkspace ipynb tex rst adoc org diff patch " +
      "wasm o a lib so dylib pdb class jar war ear wheel whl pyc pyd nupkg rpm gltf 3ds vb vbs s asm pas hxx hpp cc cxx mjs mts map less sass styl " +
      "reg sln csproj vbproj vcxproj";
    const generic = inventory.split(" ").filter((ext) => {
      const n = node(`file.${ext}`);
      return fileTypeOf(n) === null && categoryOf(n) === "other";
    });
    expect(generic).toEqual([]);
    // Sound the browser can't play keeps the audio icon, and a download instead of a player
    expect(categoryOf(node("voice.amr"))).toBe("audio");
    expect(isBrowserMedia(node("voice.amr"))).toBe(false);
  });

  test("letter case doesn't matter, and unknown files keep the generic icon", () => {
    expect(id("MAIN.RS")).toBe("rust");
    expect(id("Config.YAML")).toBe("yaml");
    expect(id("DOCKERFILE")).toBe("docker");
    expect(id("notes.customext")).toBeNull();
    expect(categoryOf(node("notes.customext"))).toBe("other");
    expect(typeTitle(node("notes.customext"))).toBe("File");
    expect(id("noextension")).toBeNull();
  });
});

describe("formats other formats' extensions or MIME types claim", () => {
  test("go.mod isn't a video, and an RPM package isn't sound", () => {
    expect(categoryOf(node("go.mod"))).toBe("other");
    expect(isBrowserMedia(node("go.mod"))).toBe(false);
    expect(categoryOf(node("tool-1.0.rpm"))).toBe("archive");
    expect(isBrowserMedia(node("tool-1.0.rpm"))).toBe(false);
    // Other .mod files are still whatever their MIME type says
    expect(categoryOf(node("song.mod"))).toBe("video");
  });

  test("Office templates and slide shows are Office files, not code to edit as text", () => {
    const names = ["letter.dotx", "budget.xltx", "deck.potx", "show.ppsx"];
    expect(names.map((name) => categoryOf(node(name)))).toEqual(["word", "sheet", "slides", "slides"]);
    expect(names.filter((name) => isTextLike(node(name)))).toEqual([]);
  });

  test("a small .ts or .mts file is TypeScript, a large one is still a video", () => {
    expect(id("app.ts", 4000)).toBe("ts");
    expect(id("worker.mts", 4000)).toBe("ts");
    expect(categoryOf(node("app.ts", 4000))).toBe("code");
    expect(id("recording.mts", 500 * 1024 * 1024)).toBeNull();
    expect(categoryOf(node("recording.mts", 500 * 1024 * 1024))).toBe("video");
    expect(categoryOf(node("broadcast.ts", MAX_SCRIPT_BYTES + 1))).toBe("video");
    // Without a size (a MIME type from the browser), only a MIME type that isn't video says it is code
    expect(fileTypeOf({ name: "clip.mts", mime: "video/mp2t" })).toBeNull();
    expect(fileTypeOf({ name: "app.ts", mime: "" })).not.toBeNull();
    // VHDL source is small, a virtual disk isn't
    expect(id("alu.vhd", 4000)).toBe("hdl");
    expect(id("server.vhd", 40 * 1024 * 1024 * 1024)).toBe("vm-disk");
    // Declarations are always TypeScript
    expect(id("types.d.ts", 50 * 1024 * 1024)).toBe("ts");
  });
});

describe("what can be done with a file", () => {
  test("an icon doesn't make a binary format previewable or editable", () => {
    const binary = "design.psd app.exe font.ttf data.sqlite disk.iso book.epub model.glb cert.pfx lib.jar pkg.deb thing.wasm table.parquet".split(" ");
    expect(binary.filter((name) => fileTypeOf(node(name)) === null)).toEqual([]);
    expect(binary.filter((name) => isTextLike(node(name)) || isBrowserMedia(node(name)))).toEqual([]);
  });

  test("files that were text before still are, and the others still aren't", () => {
    const text = ["main.rs", "main.go", "config.json", "index.js", "notes.txt", "README.md", "script.sh"];
    expect(text.filter((name) => !isTextLike(node(name)))).toEqual([]);
    const other = ["Dockerfile", "Makefile", "LICENSE", "yarn.lock", ".gitignore", "go.sum"];
    expect(other.filter((name) => isTextLike(node(name)))).toEqual([]);
    expect(isBrowserMedia(node("movie.mp4"))).toBe(true);
  });
});
