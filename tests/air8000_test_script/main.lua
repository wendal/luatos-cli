-- Air8000 test script
-- This script prints periodic messages to verify script flashing and log output

PROJECT = "test_air8000"
VERSION = "1.0.0"

sys = require("sys")

sys.taskInit(function()
    while true do
        log.info("test", "Hello from Air8000! tick=" .. tostring(os.clock()))
        sys.wait(2000)
    end
end)

sys.run()
