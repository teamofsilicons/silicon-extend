package com.teamofsilicons.extend.adb

import com.teamofsilicons.extend.driver.CommandFailure

/** Parse privileged commands separately so shell flags are never consumed as Extend flags. */
sealed interface AdbCommand {
    data class Raw(val args: List<String>) : AdbCommand
    /** `reinstall` ([fresh]) removes the app and its data first, as agent-device does. */
    data class Install(val app: String, val path: String, val fresh: Boolean) : AdbCommand
    data class Record(val action: String, val name: String = "recording", val quality: String = "normal") : AdbCommand
    data class Logs(val action: String, val label: String = "") : AdbCommand
}
object AdbCommands {
    fun parse(command: String, args: List<String>): AdbCommand {
        fun invalid(usage: String): Nothing = throw CommandFailure.invalid(usage)
        return when (command) {
            "adb" -> {
                if (args.isEmpty()) invalid("Usage: adb shell <command> | logcat <args> | push <attachment> <path> | pull <path> | install <attachment> | uninstall <package>")
                AdbCommand.Raw(args)
            }
            "install", "reinstall" -> {
                if (args.size != 2 || !args[0].matches(Regex("[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+")))
                    invalid("Usage: $command <package.name> <APK attachment>")
                AdbCommand.Install(args[0], args[1], command == "reinstall")
            }
            "record" -> {
                val action = args.firstOrNull()
                if (action == "stop" && args.size == 1) return AdbCommand.Record("stop")
                if (action != "start") invalid("Usage: record start [name] [--scope device] [--quality normal|high] | record stop")
                var name = "recording"; var quality = "normal"; var i = 1
                if (args.getOrNull(i)?.startsWith("--") == false) name = args[i++]
                while (i < args.size) {
                    val flag = args[i++]; val value = args.getOrNull(i++) ?: invalid("$flag needs a value")
                    when (flag) {
                        "--quality" -> { if (value !in setOf("normal", "high")) invalid("--quality takes normal or high"); quality = value }
                        "--scope" -> if (value != "device") throw CommandFailure.unsupported("Android debugging records the whole device; use --scope device.")
                        "--fps" -> throw CommandFailure.unsupported("Android screenrecord chooses its frame rate automatically; omit --fps.")
                        else -> invalid("Unknown recording flag $flag")
                    }
                }
                if (!name.matches(Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,100}"))) invalid("Recording name must be a simple filename (up to 101 characters).")
                AdbCommand.Record("start", name, quality)
            }
            "logs" -> {
                val action = args.firstOrNull()
                if (action !in setOf("start", "stop", "mark", "clear")) invalid("Usage: logs start|stop|mark <label>|clear")
                if (action == "mark") {
                    if (args.size < 2) invalid("logs mark needs a label")
                    AdbCommand.Logs(action, args.drop(1).joinToString(" "))
                } else {
                    if (args.size != 1) invalid("logs $action takes no additional arguments")
                    AdbCommand.Logs(action!!)
                }
            }
            else -> invalid("Unknown Android debugging command $command")
        }
    }
}
