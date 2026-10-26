PROJECT = "ec618_flash_verify"
VERSION = "1.0.0"

sys = require("sys")

sys.taskInit(function()
    while true do
        log.info("ec618_verify", "EC618_SCRIPT_OK")
        sys.wait(1000)
    end
end)

sys.run()
