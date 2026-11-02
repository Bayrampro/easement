# Changelog

## [0.4.4](https://github.com/mokevnin/easement/compare/v0.4.3...v0.4.4) (2026-11-02)


### Bug Fixes

* hash an indexer again when it is replaced in place during a run ([6d7a8a0](https://github.com/mokevnin/easement/commit/6d7a8a048dc2b4d17dfff89ca3ca5cf0a2029f7e))
* keep suggest, impact, why and baseline careful about files in `uncovered_ok` ([dce2298](https://github.com/mokevnin/easement/commit/dce22982ebad7947b7ddb3f0866c27453bb8f86d))
* keep the reasons for a partial verdict and the cache note in `--format github` ([f5fb41a](https://github.com/mokevnin/easement/commit/f5fb41a30103c492dbc66e19a972afb3a40fb9be))
* key an indexer behind rustup or a mise shim by the binary it runs ([6932249](https://github.com/mokevnin/easement/commit/69322496f39d2acc8ad646642ff5cb15c4be57fc))
* key cached indexes on the environment and say when a report used them ([3e05d01](https://github.com/mokevnin/easement/commit/3e05d01c1e0851012c2accab573e75a1bfa8266c))
* key each indexer on the variables verified to change its index ([8ac1093](https://github.com/mokevnin/easement/commit/8ac1093246805e9e3bf8d3db8a374e1eef4acaee))
* make a run narrowed by design a notice in `--format github`, not a warning ([fbfa525](https://github.com/mokevnin/easement/commit/fbfa525895e507ecbdc9d9fafffd1dab92e79208))
* say in impact, why, suggest, graph and baseline when the indexes came from the cache ([bf1f2c5](https://github.com/mokevnin/easement/commit/bf1f2c5e85ad1f677e26f50fbf3746a6940ac2c4))

## [0.4.3](https://github.com/mokevnin/easement/compare/v0.4.2...v0.4.3) (2026-11-01)


### Features

* check `defines` globs when the config loads, and name every claimant ([afb623b](https://github.com/mokevnin/easement/commit/afb623b2d2c3e5e32c9870ccac975bdb9a135632))
* check the dependencies against a Mermaid diagram with `diagram` ([c6bd32a](https://github.com/mokevnin/easement/commit/c6bd32ae8c0fdb4999812e0d6ea2cd49892b3e2e))
* claim type names for a component with `defines` ([2957d52](https://github.com/mokevnin/easement/commit/2957d520afe48c55b02138e586c63b6d229a8b65))
* fail on an import the nearest manifest does not declare ([ae59f31](https://github.com/mokevnin/easement/commit/ae59f31b97d511cca9c24fb706051c3dce3b1b87))
* judge C# and PHP in `unused`, and know Swift and PHP in `$std` ([fbfdaf0](https://github.com/mokevnin/easement/commit/fbfdaf011981e66a11a3d22008baeb3f62a25447))
* judge only the packages a file names in `manifests` ([fd44bbc](https://github.com/mokevnin/easement/commit/fd44bbcb7b0cdcdf1f6b3bdb13fe4b8e66d5a2f2))
* read every Mermaid link in `diagram`, and leave tests and common out ([c8cf81f](https://github.com/mokevnin/easement/commit/c8cf81fb631f6e889db33d93629607be798de1df))

## [0.4.2](https://github.com/mokevnin/easement/compare/v0.4.1...v0.4.2) (2026-11-01)


### Features

* report files that no entry reaches as `unused` ([52db364](https://github.com/mokevnin/easement/commit/52db3648de5a2dc2f2f95904fca4880e602cc279))

## [0.4.1](https://github.com/mokevnin/easement/compare/v0.4.0...v0.4.1) (2026-11-01)


### Features

* name the path to the field in a config error ([e29c46f](https://github.com/mokevnin/easement/commit/e29c46f0a2ca2fe650e99fd04745c40b71bb6b90))
* suggest the component a mistyped name is likely meant to be ([a9f92b7](https://github.com/mokevnin/easement/commit/a9f92b7072a094f4e30e1ec350124a007223383e))


### Bug Fixes

* key the index cache by BLAKE3 instead of `DefaultHasher` ([cd77bcd](https://github.com/mokevnin/easement/commit/cd77bcd870b939ef5576caf619fb4a545fcc2385))

## [0.4.0](https://github.com/mokevnin/easement/compare/v0.3.7...v0.4.0) (2026-11-01)


### ⚠ BREAKING CHANGES

* a checked file that imports an excluded file now fails with an `excluded` violation. Generated code the project imports belongs in `target_only`, not `exclude`.
* `easement baseline` over an existing baseline no longer records new violations; it lists them and exits 1. Pass `--accept-new` to legalize them.
* a config with a component that matches no file, which used to pass, now fails with an `unmatched` violation. This includes a component whose files are all hidden by `exclude` or `.gitignore`. Such a component fails every `easement check`, `--diff` included, until its pattern is fixed or it is marked `optional: true`.

### Features

* add `$std` for the standard library in external module rules ([d06a495](https://github.com/mokevnin/easement/commit/d06a495c41aa08b6f59588c364fa7960fcb7b37f))
* add `impact`, `why` and `graph --focus` for planning a change ([ea02ea5](https://github.com/mokevnin/easement/commit/ea02ea576367f678a6c7ed5b30db66ef5101865a))
* add `must_not_reach` to forbid a dependency through any chain ([963eacf](https://github.com/mokevnin/easement/commit/963eacf9c3e3711ac9e8fb2ce8eec5ab5d49ca8b))
* add sibling and closed layers ([c4bff86](https://github.com/mokevnin/easement/commit/c4bff861721f7b58e056c6f72dafd98f7dede7dc))
* fail on a component pattern that matches no file ([cf4a776](https://github.com/mokevnin/easement/commit/cf4a776176ac8379bdda3d52502217403ec0be32))
* fail on an import of a file that `exclude` hides ([c099eb6](https://github.com/mokevnin/easement/commit/c099eb6e34bfc4bd870cbbd2b804d3f49ffe6497))
* keep imports within one slice with named placeholders ([5ddfd18](https://github.com/mokevnin/easement/commit/5ddfd1892222df47ba1ee0f8687ff8823a22b143))
* let `$std` cover old Python and the JDK, and say when it cannot judge ([2846b44](https://github.com/mokevnin/easement/commit/2846b4453dd7ff1bc6aabe56b5051eb735aafef0))
* let `easement baseline` only shrink an existing baseline ([f20a4d7](https://github.com/mokevnin/easement/commit/f20a4d76097fc78e74b0145afca4e30cee88116e))
* name an owner for a component to ask about its rules ([2fdc23b](https://github.com/mokevnin/easement/commit/2fdc23b66e20aa6f9b73462eea4f3d6355c67a38))
* report the rules in `warn` as warnings that do not fail ([40e4791](https://github.com/mokevnin/easement/commit/40e47914d372bcbc299ff80e731416c097698d4d))
* say which layer stops a dependency, and show owners in more places ([42b801a](https://github.com/mokevnin/easement/commit/42b801a27bee3e8ed21c1ea1998d0b32fb64f4af))


### Bug Fixes

* key cycle, limit and requirement baseline entries by what they are about ([acf2f81](https://github.com/mokevnin/easement/commit/acf2f81cd9c18d10da6ba2794cac696209a9e559))
* never suggest allowing an import that the config forbids ([e445de2](https://github.com/mokevnin/easement/commit/e445de2fcb176211413c2abb1cf36d24ab59b5b1))

## [0.3.7](https://github.com/mokevnin/easement/compare/v0.3.6...v0.3.7) (2026-10-30)


### Bug Fixes

* report Ruby constants and methods to forbid_symbol and public_symbols ([#44](https://github.com/mokevnin/easement/issues/44)) ([b21f15d](https://github.com/mokevnin/easement/commit/b21f15d8353a520c36c1043c92cb09b033fd03aa))

## [0.3.6](https://github.com/mokevnin/easement/compare/v0.3.5...v0.3.6) (2026-10-29)


### Features

* add check --cache-only, which runs no indexer and exits 3 on a miss ([#42](https://github.com/mokevnin/easement/issues/42)) ([632a2c8](https://github.com/mokevnin/easement/commit/632a2c88b48b1c575197a930a914afdebdf6157c))

## [0.3.5](https://github.com/mokevnin/easement/compare/v0.3.4...v0.3.5) (2026-10-29)


### Bug Fixes

* key the index cache on the indexer's content, not its install time ([#40](https://github.com/mokevnin/easement/issues/40)) ([3067fdd](https://github.com/mokevnin/easement/commit/3067fdd2b75596d44319e51ac6ff6a60c6b74c58))

## [0.3.4](https://github.com/mokevnin/easement/compare/v0.3.3...v0.3.4) (2026-10-29)


### Features

* key each index on the files its indexer reads ([#38](https://github.com/mokevnin/easement/issues/38)) ([49d0ea5](https://github.com/mokevnin/easement/commit/49d0ea530c5eee180af6996540fd199ec948c709))

## [0.3.3](https://github.com/mokevnin/easement/compare/v0.3.2...v0.3.3) (2026-10-28)


### Bug Fixes

* build the Linux packages without the test indexers ([#35](https://github.com/mokevnin/easement/issues/35)) ([25ff897](https://github.com/mokevnin/easement/commit/25ff8979852896fbf2d80bf69dbea734d92c8bf6))

## [0.3.2](https://github.com/mokevnin/easement/compare/v0.3.1...v0.3.2) (2026-10-28)


### Features

* resolve Ruby through the scip-ruby indexer ([#33](https://github.com/mokevnin/easement/issues/33)) ([81ea7ca](https://github.com/mokevnin/easement/commit/81ea7ca5fa06d95d502c203bd8fee02ddd68b32f))

## [0.3.1](https://github.com/mokevnin/easement/compare/v0.3.0...v0.3.1) (2026-10-26)


### Features

* key the index cache on content so other checkouts reuse it ([#30](https://github.com/mokevnin/easement/issues/30)) ([a3aa08a](https://github.com/mokevnin/easement/commit/a3aa08a0ef21647c577a6e691da42ebc1f4a7e44))
* target_only keeps generated files in the graph as targets ([#32](https://github.com/mokevnin/easement/issues/32)) ([480896d](https://github.com/mokevnin/easement/commit/480896d106b680847aec82cb73a3864cc1e65590))
* uncovered_ok takes a list of globs, suggest --check fails on unused allowances ([#29](https://github.com/mokevnin/easement/issues/29)) ([5998f7d](https://github.com/mokevnin/easement/commit/5998f7d73a857b08c09eee7258cb19f0befe6331))

## [0.3.0](https://github.com/mokevnin/easement/compare/v0.2.0...v0.3.0) (2026-10-26)


### ⚠ BREAKING CHANGES

* report every use of a violation, not the first one per file ([#25](https://github.com/mokevnin/easement/issues/25))
* Java and C# projects need scip-java (with a JDK and Maven or Gradle) and scip-dotnet (with the .NET SDK) installed. The `dynamic imports not analysed` count and `stats.dynamic` are gone. Violations name the target file rather than the package or namespace.
* PHP projects need scip-php installed and `composer install` done. Functions and constants brought in with `use function` and `use const` are no longer dependencies. Violations name the target file rather than the specifier as written.

### Features

* easement doctor checks that the indexers can run ([#22](https://github.com/mokevnin/easement/issues/22)) ([b627c8e](https://github.com/mokevnin/easement/commit/b627c8e4cc66d6240da7534c14ac41cfa132d541))
* easement suggest --patch drops unused allowances as a patch ([#27](https://github.com/mokevnin/easement/issues/27)) ([b85a532](https://github.com/mokevnin/easement/commit/b85a532b091dce27537261cc428e9b96cf75d56c))
* index Go files behind build tags with go.tags ([#24](https://github.com/mokevnin/easement/issues/24)) ([a2e818d](https://github.com/mokevnin/easement/commit/a2e818dbbb37e9d8f50b649ed6827a01c0f1d0e1))
* keep the indexers' output between runs ([#20](https://github.com/mokevnin/easement/issues/20)) ([1a82708](https://github.com/mokevnin/easement/commit/1a8270869f5b7e18f35bd0f72932733e66602b4d))
* list the files no indexer covered and mark a partial verdict ([#21](https://github.com/mokevnin/easement/issues/21)) ([c1def55](https://github.com/mokevnin/easement/commit/c1def55eb0946620d5005d0560e8d0a68df9ac93))
* mark a run that judges only a part as partial ([#26](https://github.com/mokevnin/easement/issues/26)) ([231ef3a](https://github.com/mokevnin/easement/commit/231ef3a669d2d44335dccb522e9e9546f0c838dd))
* resolve Java and C# through SCIP indexers ([#18](https://github.com/mokevnin/easement/issues/18)) ([b38442f](https://github.com/mokevnin/easement/commit/b38442fa7c0a585356e77865f4682c688b1b26dd))
* resolve PHP through the scip-php indexer ([#16](https://github.com/mokevnin/easement/issues/16)) ([bbbe3ef](https://github.com/mokevnin/easement/commit/bbbe3ef693738fd5bdb2d4067ea3df69c95a20d6))
* share a part of a rule between components with YAML merge keys ([#23](https://github.com/mokevnin/easement/issues/23)) ([dcdfdcf](https://github.com/mokevnin/easement/commit/dcdfdcfbfb87d49836bfdead32ba4d6e5be7f52c))


### Bug Fixes

* report every use of a violation, not the first one per file ([#25](https://github.com/mokevnin/easement/issues/25)) ([e2e8b12](https://github.com/mokevnin/easement/commit/e2e8b125c7237e87ec37751eb8478cc2a6fb5fd4))
* suggest does not trust a missing import next to unindexed files ([#19](https://github.com/mokevnin/easement/issues/19)) ([a24f93d](https://github.com/mokevnin/easement/commit/a24f93da3c451f49a8010087f0cc1c5720b704ac))

## [0.2.0](https://github.com/mokevnin/easement/compare/v0.1.2...v0.2.0) (2026-10-25)


### ⚠ BREAKING CHANGES

* Python and Rust projects need scip-python and rust-analyzer installed, and third-party Python modules must be installed in the environment. The `python` key of easement.yml is removed. Violations name the target file rather than the specifier as written.
* Go and TypeScript projects need scip-go and scip-typescript installed (and node_modules for TypeScript). Only files a tsconfig.json includes are checked. require() in .ts files, vi.mock() and similar string arguments are no longer imports, and a violation names the target file rather than the specifier as written.

### Features

* file and directive rules, Go symbol rules, tests: auto, suggest and diff summary ([#11](https://github.com/mokevnin/easement/issues/11)) ([597bb5c](https://github.com/mokevnin/easement/commit/597bb5c7b87907fc4a025c9c265e104ac10f671c))
* resolve Go and TypeScript through SCIP indexers ([#13](https://github.com/mokevnin/easement/issues/13)) ([3602fc8](https://github.com/mokevnin/easement/commit/3602fc86ae98ec964b02ab7209a5a5425d9c2554))
* resolve Python and Rust through SCIP indexers ([#14](https://github.com/mokevnin/easement/issues/14)) ([b25d74f](https://github.com/mokevnin/easement/commit/b25d74f5fa63b9daceaf49aae8755a15f3a6f007))
* shell completions and coloured output ([#15](https://github.com/mokevnin/easement/issues/15)) ([92d4472](https://github.com/mokevnin/easement/commit/92d4472fd952c2535c8f95a27bfa788fe14e9744))

## [0.1.2](https://github.com/mokevnin/easement/compare/v0.1.1...v0.1.2) (2026-10-25)


### Features

* isolated instances, importer lists, priority, scopes and required imports ([#8](https://github.com/mokevnin/easement/issues/8)) ([afa3221](https://github.com/mokevnin/easement/commit/afa3221b22d11eeba075478ce286d4215f8b0a3b))

## [0.1.1](https://github.com/mokevnin/easement/compare/v0.1.0...v0.1.1) (2026-10-25)


### Features

* external, layer and metric rules, five more languages, CI formats and tooling ([#4](https://github.com/mokevnin/easement/issues/4)) ([abafd9d](https://github.com/mokevnin/easement/commit/abafd9d3d440ca7f07da06ab731b2a37e8380708))

## 0.1.0 (2026-10-25)


### Features

* diff mode for pull requests ([b79cd2f](https://github.com/mokevnin/easement/commit/b79cd2fb0fb9c97d774e58d2f75ef550464f8c55))
* install script and .deb/.rpm packages ([#2](https://github.com/mokevnin/easement/issues/2)) ([1c5a1df](https://github.com/mokevnin/easement/commit/1c5a1dfb9ad14086de629b479401e7dd5e2e0af6))
* public API, cycle detection, rule reasons and inline suppressions ([abfa525](https://github.com/mokevnin/easement/commit/abfa52578faac44d0f569ae67a7f4988c92b7d03))


### Bug Fixes

* **deps:** replace deprecated serde_yaml with serde_norway ([5ac7456](https://github.com/mokevnin/easement/commit/5ac7456586fa18b9f0f99b8272cbb77528ce20fa))
