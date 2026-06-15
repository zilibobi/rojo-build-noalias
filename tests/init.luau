local fs = require("@lune/fs")
local serde = require("@lune/serde")
local process = require("@lune/process")
local stdio = require("@lune/stdio")

local TESTS_PASSED = 0
local TESTS_FAILED = 0
local TESTS_TOTAL = 0

local tempBase = process.env.TEMP or process.env.TMPDIR or "/tmp"
tempBase = string.gsub(tempBase, "\\", "/")
tempBase = string.gsub(tempBase, "/+$", "")
local TEMP_DIR = tempBase .. "/rbn-tests"
local TOOL_PATH = `{process.cwd}/src/init.luau`
local FIXTURES_DIR = `{process.cwd}/tests/fixtures`

-- helpers

local function cleanTemp()
  if fs.isDir(TEMP_DIR) then
    fs.removeDir(TEMP_DIR)
  end
  fs.writeDir(TEMP_DIR)
end

local function copyDir(from: string, to: string)
  fs.writeDir(to)
  for _, entry in fs.readDir(from) do
    local fromPath = `{from}/{entry}`
    local toPath = `{to}/{entry}`
    if fs.isDir(fromPath) then
      copyDir(fromPath, toPath)
    else
      fs.writeFile(toPath, fs.readFile(fromPath))
    end
  end
end

local function readFile(path: string): string
  return fs.readFile(path)
end

local function fileExists(path: string): boolean
  return fs.isFile(path)
end

local function dirExists(path: string): boolean
  return fs.isDir(path)
end

local function setupFixture(name: string): string
  local fixtureDir = `{FIXTURES_DIR}/{name}`
  local workDir = `{TEMP_DIR}/{name}`

  if not fs.isDir(fixtureDir) then
    error(`Fixture not found: {fixtureDir}`)
  end

  copyDir(fixtureDir, workDir)
  return workDir
end

local function runConvert(projectDir: string, outputDir: string): (boolean, string)
  local res = process.exec("lune", {
    "run",
    TOOL_PATH,
    "--",
    "--convert-to",
    outputDir,
  }, { cwd = projectDir })

  return res.ok, res.stdout .. res.stderr
end

local function runConvertWithOrphans(projectDir: string, outputDir: string): (boolean, string)
  local res = process.exec("lune", {
    "run",
    TOOL_PATH,
    "--",
    "--convert-to",
    outputDir,
    "--process-orphans",
  }, { cwd = projectDir })

  return res.ok, res.stdout .. res.stderr
end

local function runConvertWithWaxCompat(projectDir: string, outputDir: string): (boolean, string)
  local res = process.exec("lune", {
    "run",
    TOOL_PATH,
    "--",
    "--convert-to",
    outputDir,
    "--wax-compat",
  }, { cwd = projectDir })

  return res.ok, res.stdout .. res.stderr
end

local function runBuild(projectDir: string, outputPath: string): (boolean, string)
  local res = process.exec("lune", {
    "run",
    TOOL_PATH,
    "--",
    "-o",
    outputPath,
  }, { cwd = projectDir })

  return res.ok, res.stdout .. res.stderr
end

local function assertEq(actual: any, expected: any, msg: string)
  if actual ~= expected then
    error(`{msg}: expected {tostring(expected)}, got {tostring(actual)}`)
  end
end

local function assertContains(haystack: string, needle: string, msg: string)
  if not string.find(haystack, needle, 1, true) then
    error(`{msg}: expected to find "{needle}" in:\n{haystack}`)
  end
end

local function assertNotContains(haystack: string, needle: string, msg: string)
  if string.find(haystack, needle, 1, true) then
    error(`{msg}: expected NOT to find "{needle}" in:\n{haystack}`)
  end
end

local function test(name: string, fn: () -> ())
  TESTS_TOTAL += 1
  local ok, err = pcall(fn)
  if ok then
    TESTS_PASSED += 1
    stdio.write(stdio.color("green") .. `  PASS ` .. stdio.color("reset") .. `{name}\n`)
  else
    TESTS_FAILED += 1
    stdio.write(stdio.color("red") .. `  FAIL ` .. stdio.color("reset") .. `{name}\n`)
    stdio.write(stdio.color("red") .. `       {err}\n` .. stdio.color("reset"))
  end
end

-- tests

print("\nRunning rojo-build-noalias tests...\n")

-- ============================================================
-- Basic alias resolution
-- ============================================================

print(stdio.color("cyan") .. "  Alias Resolution" .. stdio.color("reset"))

test("basic @alias requires are replaced with relative paths", function()
  cleanTemp()
  local projectDir = setupFixture("basic-alias")
  local outputDir = `{TEMP_DIR}/basic-alias-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- init.luau: @mod/helpers and @utils/config should become @self paths
  local init = readFile(`{outputDir}/src/init.luau`)
  assertContains(init, 'require("@self/mod/helpers")', "init.luau: @mod/helpers -> @self/mod/helpers")
  assertContains(init, 'require("@self/utils/config")', "init.luau: @utils/config -> @self/utils/config")
  assertNotContains(init, "@mod/", "init.luau should not contain @mod alias")
  assertNotContains(init, "@utils/", "init.luau should not contain @utils alias")

  -- mod/helpers.luau: @utils/config should become relative
  local helpers = readFile(`{outputDir}/src/mod/helpers.luau`)
  assertContains(helpers, 'require("../utils/config")', "helpers.luau: @utils/config -> ../utils/config")
  assertNotContains(helpers, "@utils/", "helpers.luau should not contain @utils alias")
end)

test("--wax-compat adds extra `..` to nested init.luau requires", function()
  cleanTemp()
  local projectDir = setupFixture("nested-init")
  local outputDir = `{TEMP_DIR}/nested-init-wax-out`

  local ok, output = runConvertWithWaxCompat(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- Under wax-compat, nested init.luau gets one extra `..` so wax's
  -- pre-RFC shim (init script is its own folder for `./` and `../`)
  -- walks the same number of instance-tree levels as a sibling leaf.
  local nestedInit = readFile(`{outputDir}/src/lib/Foo/init.luau`)
  assertContains(nestedInit, 'require("../../pkg/Logger")', "wax-compat nested init: @pkg/Logger -> ../../pkg/Logger")
  assertNotContains(nestedInit, 'require("../pkg/Logger")', "wax-compat must NOT emit Luau-spec path")
end)

test("nested init.luau resolves @ aliases per Luau abstract-module-paths spec", function()
  cleanTemp()
  local projectDir = setupFixture("nested-init")
  local outputDir = `{TEMP_DIR}/nested-init-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- root init still gets @self conversion
  local rootInit = readFile(`{outputDir}/src/init.luau`)
  assertContains(rootInit, 'require("@self/lib/Foo")', "root init: @lib/Foo -> @self/lib/Foo")
  assertContains(rootInit, 'require("@self/pkg/Logger")', "root init: @pkg/Logger -> @self/pkg/Logger")

  -- Per Luau RFC (abstract-module-paths-and-init.luau, Implemented):
  -- `./X` from foo/init.luau resolves to the OUTER X (sibling of foo), not
  -- a child of foo. So @pkg/Logger from src/lib/Foo/init.luau is 1 hop:
  -- Foo's context is `lib`; `..` from there reaches the project root, then
  -- into pkg/Logger.
  local nestedInit = readFile(`{outputDir}/src/lib/Foo/init.luau`)
  assertContains(nestedInit, 'require("../pkg/Logger")', "nested init: @pkg/Logger -> ../pkg/Logger")
  assertNotContains(nestedInit, 'require("../../pkg/Logger")', "nested init must NOT emit extra-`..` path")
end)

test("@self alias is resolved correctly", function()
  cleanTemp()
  local projectDir = setupFixture("self-alias")
  local outputDir = `{TEMP_DIR}/self-alias-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- init.luau: @self/mod/helpers should resolve to @self path
  local init = readFile(`{outputDir}/src/init.luau`)
  assertContains(init, 'require("@self/mod/helpers")', "init.luau: @self/mod/helpers preserved")
  assertContains(init, 'require("@self/mod/deep/nested")', "init.luau: @self/mod/deep/nested preserved")

  -- mod/helpers.luau: @self/mod/deep/nested should become relative
  -- (goes up to alias root SelfAlias, then back down via subpath)
  local helpers = readFile(`{outputDir}/src/mod/helpers.luau`)
  assertContains(helpers, 'require("../mod/deep/nested")', "helpers.luau: @self/mod/deep/nested -> ../mod/deep/nested")
end)

-- ============================================================
-- External $path flattening (--convert-to)
-- ============================================================

print(stdio.color("cyan") .. "\n  External $path Flattening" .. stdio.color("reset"))

test("external $path is flattened into root $path", function()
  cleanTemp()
  local projectDir = setupFixture("external-path")
  local outputDir = `{TEMP_DIR}/external-path-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- pkg/ should be moved inside src/
  assertEq(dirExists(`{outputDir}/src/pkg`), true, "pkg should be moved into src/")
  assertEq(fileExists(`{outputDir}/src/pkg/Promise.luau`), true, "Promise.luau should exist in src/pkg/")
  assertEq(fileExists(`{outputDir}/src/pkg/Signal.luau`), true, "Signal.luau should exist in src/pkg/")
  assertEq(dirExists(`{outputDir}/pkg`), false, "pkg/ should not exist at root level")

  -- requires should resolve correctly on disk
  local helpers = readFile(`{outputDir}/src/mod/helpers.luau`)
  assertContains(helpers, 'require("../pkg/Promise")', "helpers.luau: @pkg/Promise -> ../pkg/Promise")

  -- init.luau uses @self for pkg since it's a child of root
  local init = readFile(`{outputDir}/src/init.luau`)
  assertContains(init, 'require("@self/pkg/Promise")', "init.luau: @pkg/Promise -> @self/pkg/Promise")

  -- project.json should be updated
  local project = serde.decode("json", readFile(`{outputDir}/default.project.json`))
  assertEq(project.tree.pkg["$path"], "src/pkg", "project.json pkg $path should be src/pkg")
end)

test("multiple external $path entries are all flattened", function()
  cleanTemp()
  local projectDir = setupFixture("multi-external-path")
  local outputDir = `{TEMP_DIR}/multi-external-path-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- both pkg/ and vendor/ should be inside src/
  assertEq(dirExists(`{outputDir}/src/pkg`), true, "pkg should be inside src/")
  assertEq(dirExists(`{outputDir}/src/vendor`), true, "vendor should be inside src/")
  assertEq(fileExists(`{outputDir}/src/pkg/Promise.luau`), true, "Promise.luau in src/pkg/")
  assertEq(fileExists(`{outputDir}/src/vendor/Lib.luau`), true, "Lib.luau in src/vendor/")
  assertEq(dirExists(`{outputDir}/pkg`), false, "pkg/ should not exist at root")
  assertEq(dirExists(`{outputDir}/vendor`), false, "vendor/ should not exist at root")

  -- requires from mod/utils.luau should be relative
  local utils = readFile(`{outputDir}/src/mod/utils.luau`)
  assertContains(utils, 'require("../pkg/Promise")', "utils: @pkg/Promise -> ../pkg/Promise")
  assertContains(utils, 'require("../vendor/Lib")', "utils: @vendor/Lib -> ../vendor/Lib")

  -- project.json should be updated for both
  local project = serde.decode("json", readFile(`{outputDir}/default.project.json`))
  assertEq(project.tree.pkg["$path"], "src/pkg", "pkg $path updated")
  assertEq(project.tree.vendor["$path"], "src/vendor", "vendor $path updated")
end)

test("child $path already inside root is not moved", function()
  cleanTemp()
  local projectDir = setupFixture("nested-inside-path")
  local outputDir = `{TEMP_DIR}/nested-inside-path-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- lib should stay at src/lib, not get moved to src/src/lib
  assertEq(fileExists(`{outputDir}/src/lib/foo.luau`), true, "foo.luau should be at src/lib/")
  assertEq(dirExists(`{outputDir}/src/src`), false, "should not create src/src/")

  -- project.json should be unchanged
  local project = serde.decode("json", readFile(`{outputDir}/default.project.json`))
  assertEq(project.tree.lib["$path"], "src/lib", "lib $path should remain src/lib")
end)

-- ============================================================
-- --convert-to output structure
-- ============================================================

print(stdio.color("cyan") .. "\n  Convert-To Output Structure" .. stdio.color("reset"))

test("--convert-to copies all files and resolves aliases", function()
  cleanTemp()
  local projectDir = setupFixture("convert-to-structure")
  local outputDir = `{TEMP_DIR}/convert-to-structure-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- Source files should be copied
  assertEq(fileExists(`{outputDir}/src/init.luau`), true, "src/init.luau should exist")
  assertEq(fileExists(`{outputDir}/src/mod/helpers.luau`), true, "src/mod/helpers.luau should exist")

  -- Non-luau files should also be copied
  assertEq(fileExists(`{outputDir}/extra.txt`), true, "extra.txt should be copied")
  assertContains(readFile(`{outputDir}/extra.txt`), "this file should be copied too", "extra.txt content preserved")

  -- Project file should exist
  assertEq(fileExists(`{outputDir}/default.project.json`), true, "project.json should exist")

  -- .luaurc should be copied (it's part of the project)
  assertEq(fileExists(`{outputDir}/.luaurc`), true, ".luaurc should exist in output")

  -- rokit.toml should be copied
  assertEq(fileExists(`{outputDir}/rokit.toml`), true, "rokit.toml should exist in output")

  -- Aliases should be resolved in output
  local init = readFile(`{outputDir}/src/init.luau`)
  assertNotContains(init, "@mod/", "aliases should be resolved in output")
  assertNotContains(init, "@pkg/", "aliases should be resolved in output")

  -- pkg should be flattened into src
  assertEq(dirExists(`{outputDir}/src/pkg`), true, "pkg should be inside src/")
  assertEq(dirExists(`{outputDir}/pkg`), false, "pkg/ should not exist at root")
end)

-- ============================================================
-- --process-orphans mode
-- ============================================================

print(stdio.color("cyan") .. "\n  Orphan Mode" .. stdio.color("reset"))

test("--process-orphans replaces aliases via direct text replacement", function()
  cleanTemp()
  local projectDir = setupFixture("orphan-mode")
  local outputDir = `{TEMP_DIR}/orphan-mode-out`

  local ok, output = runConvertWithOrphans(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- Aliases should be replaced with literal paths from .luaurc
  local main = readFile(`{outputDir}/src/main.luau`)
  assertContains(main, 'require("./packages/Promise")', "main.luau: @packages/Promise -> ./packages/Promise")
  assertContains(main, 'require("./lib/utils")', "main.luau: @lib/utils -> ./lib/utils")

  -- Nested files should also be replaced
  local nested = readFile(`{outputDir}/src/deep/nested.luau`)
  assertContains(nested, 'require("./packages/Promise")', "nested.luau: @packages/Promise -> ./packages/Promise")
end)

test("without --process-orphans, orphan .luaurc files are skipped", function()
  cleanTemp()
  local projectDir = setupFixture("orphan-mode")
  local outputDir = `{TEMP_DIR}/orphan-mode-no-flag-out`

  local ok, output = runConvert(projectDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- Aliases should NOT be replaced (no project file, no --process-orphans)
  local main = readFile(`{outputDir}/src/main.luau`)
  assertContains(main, "@packages/Promise", "main.luau should still have @packages alias")
  assertContains(main, "@lib/utils", "main.luau should still have @lib alias")
end)

-- ============================================================
-- Normal build mode
-- ============================================================

print(stdio.color("cyan") .. "\n  Normal Build Mode" .. stdio.color("reset"))

test("normal build mode does not flatten external $path dirs", function()
  cleanTemp()
  local projectDir = setupFixture("normal-build")
  local outputPath = `{TEMP_DIR}/normal-build.rbxm`

  -- Run the build (may fail due to rojo process.create resolution, but
  -- the important thing is that the tool's alias replacement + temp workspace
  -- does NOT flatten the $path dirs like --convert-to does)
  runBuild(projectDir, outputPath)

  -- The original fixture should NOT be modified
  assertEq(dirExists(`{projectDir}/pkg`), true, "pkg/ should still exist in original")
  assertEq(dirExists(`{projectDir}/src/pkg`), false, "pkg should NOT be moved into src/ during normal build")

  -- The temp workspace should not have pkg flattened either
  -- (flattenTreePaths only runs in convert mode)
  local tempWorkspace = tempBase .. "/rojo-build-noalias/workspace"
  if dirExists(tempWorkspace) then
    assertEq(dirExists(`{tempWorkspace}/pkg`), true, "pkg/ should exist in temp workspace")
    assertEq(dirExists(`{tempWorkspace}/src/pkg`), false, "pkg should NOT be flattened in temp workspace")
  end
end)

-- ============================================================
-- Integration: real forge-vfx project
-- ============================================================

print(stdio.color("cyan") .. "\n  Integration" .. stdio.color("reset"))

test("real forge-vfx project converts correctly", function()
  local forgeDir = "/home/zili/laptop/vfx-forge/pkg/forge-vfx"
  if not fs.isDir(forgeDir) then
    stdio.write(stdio.color("yellow") .. `       (skipped: {forgeDir} not found)\n` .. stdio.color("reset"))
    return
  end

  cleanTemp()
  local outputDir = `{TEMP_DIR}/forge-out`

  local ok, output = runConvert(forgeDir, outputDir)
  assertEq(ok, true, `convert should succeed:\n{output}`)

  -- pkg/ should be inside src/ after flatten
  assertEq(dirExists(`{outputDir}/src/pkg`), true, "pkg should be inside src/")
  assertEq(fileExists(`{outputDir}/src/pkg/Promise.luau`), true, "Promise.luau in src/pkg/")
  assertEq(fileExists(`{outputDir}/src/pkg/Shake.luau`), true, "Shake.luau in src/pkg/")
  assertEq(dirExists(`{outputDir}/pkg`), false, "pkg/ should not exist at root")

  -- Check specific require resolutions
  local utility = readFile(`{outputDir}/src/mod/utility.luau`)
  assertContains(utility, 'require("../pkg/Promise")', "utility.luau: ../pkg/Promise")

  local camshake = readFile(`{outputDir}/src/effects/camera_shake.luau`)
  assertContains(camshake, 'require("../pkg/Shake")', "camera_shake.luau: ../pkg/Shake")

  local types = readFile(`{outputDir}/src/types.luau`)
  assertContains(types, 'require("./pkg/Promise")', "types.luau: ./pkg/Promise")
end)

-- ============================================================
-- Summary
-- ============================================================

print("")
if TESTS_FAILED > 0 then
  stdio.write(stdio.color("red"))
  print(`{TESTS_FAILED}/{TESTS_TOTAL} tests failed`)
  stdio.write(stdio.color("reset"))
  process.exit(1)
else
  stdio.write(stdio.color("green"))
  print(`All {TESTS_TOTAL} tests passed`)
  stdio.write(stdio.color("reset"))
end

-- cleanup
if fs.isDir(TEMP_DIR) then
  fs.removeDir(TEMP_DIR)
end
