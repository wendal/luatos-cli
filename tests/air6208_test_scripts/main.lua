-- Air6208 flash test from luatos-cli
PROJECT = "luatos-cli-test"
VERSION = "1.0.0"

sys = require("sys")

sys.taskInit(function()
    while true do
        log.info("luatos-cli", "hello from luatos-cli!", os.clock())
        sys.wait(2000)
    end
end)

sys.run()
