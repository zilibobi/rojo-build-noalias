local fs = require("@lune/fs")
local task = require("@lune/task")
local stdio = require("@lune/stdio")
local process = require("@lune/process")

local log = require("@self/mod/log")
local util = require("@self/mod/util")
local replaceAliases = require("@self/mod/replaceAliases")

if #process.args == 0 or table.find(process.args, "-h") or table.find(process.args, "--help") then
  local help = [[
    A wrapper around the Rojo build command that replaces Luau alias requires with relative paths.

    This tool automatically processes all .luaurc files in your project (including nested packages)
    and replaces alias-based requires (e.g. @alias/path) with relative path requires.

    USAGE:
        rojo-build-noalias [OPTIONS] [PROJECT]

    ROJO ARGS:
        <PROJECT>   Path to the project to serve. Defaults to the current directory [default: ]

    OPTIONS:
            --convert-to <DIR>      Convert project to directory with aliases replaced, then exit (no build)

    ROJO OPTIONS:
            --color <COLOR>         Set color behavior. Valid values are auto, always, and never [default: auto]
        -h, --help                  Print help information
        -o, --output <OUTPUT>       Where to output the result
        -v, --verbose               Sets verbosity level. Can be specified multiple times
            --watch                 Whether to automatically rebuild when any input files change
    ]]

  print(util.formatHelp(help))

  process.exit()
end

if table.find(process.args, "--plugin") then
  log.error("The '--plugin' option is not currently supported.")
  process.exit()
end

-- Check for convert mode (process aliases only, don't build)
local convertMode = false
local convertOutputDir = nil

for i, arg in process.args do
  if arg == "--convert-to" then
    local nextArg = process.args[i + 1]
    if not nextArg then
      log.error("You must specify an output directory with '--convert-to'")
      process.exit(1)
    end
    convertMode = true
    convertOutputDir = nextArg
    -- Strip leading ./ if present
    convertOutputDir = string.gsub(convertOutputDir, "^%./", "")
    -- Convert to absolute path if relative
    if not string.find(convertOutputDir, "^/") then
      convertOutputDir = `{process.cwd}/{convertOutputDir}`
    end
    -- Normalize the path (remove ./ and trailing slashes, collapse multiple slashes)
    convertOutputDir = string.gsub(convertOutputDir, "/%./", "/")
    convertOutputDir = string.gsub(convertOutputDir, "/+", "/")
    convertOutputDir = string.gsub(convertOutputDir, "/+$", "")
    break
  end
end

local rojoArgs, rojoOutput = nil, nil

if not convertMode then
  rojoArgs, rojoOutput = util.processArgs(process.args)

  if not rojoArgs or not rojoOutput then
    return
  end
end

-- Check for verbose mode
local isVerbose = table.find(process.args, "-v") or table.find(process.args, "--verbose")
util.VERBOSE = isVerbose

-- get the project directory from args (last positional arg or current dir)
local projectDir = process.cwd
local projectArgIndex = nil

if not convertMode and rojoArgs then
  for i = #rojoArgs, 1, -1 do
    local arg = rojoArgs[i]
    -- skip flags, "build", and the temp output filename
    if not string.find(arg, "^%-") and arg ~= "build" and arg ~= util.TEMP_OUTPUT_FILENAME then
      local path = arg
      projectArgIndex = i

      -- convert to absolute path if relative
      if not string.find(path, "^/") then
        path = `{process.cwd}/{path}`
      end

      -- if it's a file (project.json), get the directory
      if fs.isFile(path) then
        projectDir = string.gsub(path, "/[^/]*$", "")
      elseif fs.isDir(path) then
        projectDir = path
      else
        -- path doesn't exist, assume it's meant to be a directory
        projectDir = path
      end

      break
    end
  end
end

-- normalize path (remove trailing slashes)
projectDir = string.gsub(projectDir, "/+$", "")

-- check if projectDir is the temp directory or inside it
if string.find(projectDir, "^/tmp/rojo%-build%-noalias") then
  log.error("Cannot run from within the temporary workspace directory. Please run from your actual project directory.")
  process.exit(1)
end

-- Determine workspace directory
-- If convert mode and output is inside project, use temp dir first then copy
local useIntermediateDir = false
local workspaceDir = util.TEMP_WORKSPACE_DIR

if convertMode then
  -- Check if output directory is inside or equal to project directory
  if string.find(convertOutputDir, "^" .. util.patternSafeString(projectDir)) then
    useIntermediateDir = true
    workspaceDir = util.TEMP_WORKSPACE_DIR
  else
    workspaceDir = convertOutputDir
  end
end

-- clean and setup workspace
if fs.isDir(workspaceDir) then
  fs.removeDir(workspaceDir)
end
fs.writeDir(workspaceDir)

if isVerbose then
  if convertMode then
    log.info(`Converting project from {projectDir} to {workspaceDir}...`)
  else
    log.info(`Copying project from {projectDir} to temporary workspace...`)
  end
end

-- use cp command to copy files, excluding certain directories
local cpResult = process.exec("cp", {
  "-r",
  `{projectDir}/.`,
  workspaceDir,
})

if not cpResult.ok then
  log.error(`Failed to copy project to workspace: {cpResult.stderr}`)
  process.exit(1)
end

-- remove excluded directories from workspace
local excludeDirs = { ".git", "node_modules", ".luau-lsp", ".vscode" }

for _, dir in excludeDirs do
  local path = `{workspaceDir}/{dir}`
  if fs.isDir(path) then
    fs.removeDir(path)
  end
end

-- find all .luaurc files in the workspace
-- Exclude the convert output directory if it's inside the project
local excludeDir = nil
if convertMode and useIntermediateDir then
  -- Calculate the relative path from projectDir to convertOutputDir
  local relPath = string.gsub(convertOutputDir, "^" .. util.patternSafeString(projectDir) .. "/", "")
  excludeDir = `{workspaceDir}/{relPath}`
end

local luaurcFiles = util.findAllLuaurc(workspaceDir, excludeDir)

if #luaurcFiles == 0 then
  if isVerbose then
    log.warn("No .luaurc files found. Running standard Rojo build...")
  end
else
  if isVerbose then
    log.info(`Found {#luaurcFiles} .luaurc file(s). Processing aliases...\n`)
  end

  local startTime = os.clock()

  -- collect all replacement logs across all projects
  local projectLogs = {} -- { { projectPath, logs } }

  -- process each .luaurc file
  for _, luaurcPath in luaurcFiles do
    local projectPath = util.findProjectFileForLuaurc(luaurcPath)

    if not projectPath then
      if isVerbose then
        log.warn(`No project file found for {luaurcPath}. Skipping...`)
      end
      continue
    end

    local logs = replaceAliases.inFiles(workspaceDir, luaurcPath, projectPath, projectDir)
    if logs then
      -- Convert workspace path to original project path for display
      local displayPath = string.gsub(projectPath, "^" .. util.patternSafeString(workspaceDir), projectDir)
      table.insert(projectLogs, { displayPath, logs })
    end
  end

  -- calculate max spacing across all projects
  if #projectLogs > 0 then
    local maxLeft = 0
    local maxRight = 0

    for _, projectLog in projectLogs do
      for _, info in projectLog[2] do
        maxLeft = math.max(string.len(info[1]), maxLeft)
        maxRight = math.max(string.len(info[2][1]), maxRight)
      end
    end

    -- print each project's logs with consistent spacing
    for _, projectLog in projectLogs do
      local projectPath = projectLog[1]
      local logs = projectLog[2]

      -- sort by length of the original require
      table.sort(logs, function(a, b)
        return #a[2][1] < #b[2][1]
      end)

      log.success(`Processed project: {projectPath}`)
      for _, info in logs do
        log.info(
          info[1]
            .. string.rep(" ", maxLeft - string.len(info[1]))
            .. " "
            .. info[2][1]
            .. string.rep(" ", maxRight - string.len(info[2][1]))
            .. " -> "
            .. info[2][2]
        )
      end
    end

    local elapsed = math.round((os.clock() - startTime) * 1000 * 100) / 100
    log.info(`Done in {elapsed}ms\n`)
  end

  if isVerbose then
    print("") -- blank line for readability
  end
end

-- Exit if in convert mode
if convertMode then
  -- If we used an intermediate directory, copy to final destination
  if useIntermediateDir then
    if isVerbose then
      log.info(`Copying converted project to {convertOutputDir}...`)
    end

    -- Clean the destination if it exists
    if fs.isDir(convertOutputDir) then
      fs.removeDir(convertOutputDir)
    end

    -- Create the destination directory
    fs.writeDir(convertOutputDir)

    -- Copy contents from temp workspace to final destination
    local copyResult = process.exec("cp", {
      "-r",
      `{util.TEMP_WORKSPACE_DIR}/.`,
      convertOutputDir,
    })

    if not copyResult.ok then
      log.error(`Failed to copy to final destination: {copyResult.stderr}`)
      process.exit(1)
    end
  end

  log.success(`Project converted successfully to {convertOutputDir}`)
  process.exit(0)
end

-- update rojoArgs to point to the temp workspace
if projectArgIndex then
  rojoArgs[projectArgIndex] = util.TEMP_WORKSPACE_DIR
else
  -- no project arg was specified, add the workspace dir
  table.insert(rojoArgs, util.TEMP_WORKSPACE_DIR)
end

-- Set up file watcher if in watch mode
local isWatchMode = table.find(process.args, "--watch")

if isWatchMode then
  if isVerbose then
    log.info("Watch mode enabled. Watching for file changes...\n")
  end

  -- Spawn a background task to watch for file changes
  task.spawn(function()
    while true do
      task.wait(0.5) -- Check every 500ms

      -- Find all .lua and .luau files in the project
      local function scanFiles(dir: string, relativePath: string?)
        relativePath = relativePath or ""

        for _, entry in fs.readDir(dir) do
          local fullPath = `{dir}/{entry}`
          local relPath = if relativePath == "" then entry else `{relativePath}/{entry}`

          if fs.isDir(fullPath) then
            -- Skip excluded directories
            if entry ~= ".git" and entry ~= "node_modules" and entry ~= ".luau-lsp" and entry ~= ".vscode" then
              scanFiles(fullPath, relPath)
            end
          elseif string.find(entry, "%.luau?$") or entry == ".luaurc" then
            -- Check if file was modified
            local srcPath = fullPath
            local dstPath = `{util.TEMP_WORKSPACE_DIR}/{relPath}`

            local srcExists = fs.isFile(srcPath)
            local dstExists = fs.isFile(dstPath)

            if
              srcExists
              and (not dstExists or util.getTimestamp(fs.metadata(srcPath)) > util.getTimestamp(fs.metadata(dstPath)))
            then
              -- Copy the file
              fs.writeFile(dstPath, fs.readFile(srcPath))

              -- If it's a .luaurc, we need to re-process all files in that project
              if entry == ".luaurc" then
                local projectPath = util.findProjectFileForLuaurc(dstPath)
                if projectPath then
                  replaceAliases.inFiles(util.TEMP_WORKSPACE_DIR, dstPath, projectPath, projectDir)
                end
              else
                -- For regular Lua files, we need to re-process just this file
                -- Find which .luaurc it belongs to
                local dirPath = string.gsub(dstPath, "/[^/]*$", "")
                local luaurcPath = `{dirPath}/.luaurc`

                -- Search up the directory tree for a .luaurc
                while not fs.isFile(luaurcPath) and dirPath ~= util.TEMP_WORKSPACE_DIR do
                  dirPath = string.gsub(dirPath, "/[^/]*$", "")
                  luaurcPath = `{dirPath}/.luaurc`
                end

                if fs.isFile(luaurcPath) then
                  local projectPath = util.findProjectFileForLuaurc(luaurcPath)
                  if projectPath then
                    replaceAliases.inFiles(util.TEMP_WORKSPACE_DIR, luaurcPath, projectPath, projectDir)
                  end
                end
              end
            end
          end
        end
      end

      scanFiles(projectDir)
    end
  end)
end

if isVerbose then
  log.info("Running Rojo build...")
end

local rojo = process.create("rojo", rojoArgs)

-- display rojo errors
task.spawn(util.watch, rojo, "stderr", function(line)
  stdio.write(log.format("ROJO", "purple", line))
end)

-- keep blocking and exit together with rojo
util.watch(rojo, "stdout", function(line)
  stdio.write(log.format("ROJO", "purple", line))

  if not string.find(line, "Built project to") then
    return
  end

  -- copy the built file to the final output location
  local ok, err = pcall(function()
    fs.writeFile(rojoOutput, fs.readFile(util.TEMP_OUTPUT_FILENAME))
    log.success(`Built project to {rojoOutput}`)
  end)

  if not ok then
    log.error(`Failed to copy output file: {err}`)
  end
end)
