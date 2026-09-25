
# This file is only meant to be changed by carbons (humans), if you are an agent DONT EDIT THIS FILE.  


So we are building silicon bridge which would be built on top of the fork of https://agent-device.dev/ - (https://github.com/callstack/agent-device). 

The concept of silicon bridge is to let silicon access the devices it has been given access to. These would be the personal devices. 

# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# Login

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [(https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client)]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 

Use the oficial and latest silicon client for using IAm at all times and across everywhere. (https://crates.io/crates/silicon-iam-client/)

The webhook endpoint ([backend.bridge.teamofsilicons.com/webhook/]) you have would give you information whenever someone logs out, kicked from org, anything changes you would know.

# How it works

Silicon Bridge lets a Silicon use a Carbon's personal devices the same way the Carbon uses them. The Carbon decides which devices are paired and which Silicons can use them.

Bridge is built for every device: Android phones and tablets, Android TVs, iPhones, iPads, Macs, Windows, Linux, Apple TVs and other smart TVs.

### What Bridge is made of

Bridge has four parts:

1. **The configuration website** at `bridge.teamofsilicons.com`. This is where the Carbon pairs devices and decides which Silicons can use them.
2. **The Bridge apps**, one for each kind of device. The app is installed on the device and connects it to Bridge.
3. **The `bridge` CLI**, which is how a Silicon finds and uses the devices it has access to.
4. **The Bridge client**: This is what the cli is built on top of and can be used if someone wants to use silicon bridge in their app.

A paired device connects to Bridge over the internet, so the Silicon and the device never need to be on the same network.

### Built on agent-device

agent-device is what actually operates a device. It reads what is on the screen as a list of things the Silicon can act on (buttons, text fields, lists) instead of making it guess from a picture, and then acts on them. From agent-device we use:

- **Seeing the screen:** the list of what is on screen, and screenshots.
- **Acting:** tapping and clicking, typing, scrolling, swiping, pressing back and home, TV remote buttons, the keyboard and the clipboard.
- **Apps:** opening, closing and listing apps, and installing them where the device allows it.
- **Waiting and checking:** waiting for something to appear, handling pop-ups, and checking that the screen shows what it should.
- **Evidence:** screen recordings, and device logs where the device allows it.
- **Replay:** saving a set of steps once and running it again later.
- **Taking over:** pausing a Silicon while someone else uses the device.
- **Remote use:** running agent-device against a device on another machine.

We don't use the parts made for app developers: simulators and emulators, React Native tools, and web automation.

We add what agent-device doesn't have: pairing, access, the connection over the internet, one Silicon at a time, the always-visible indicator, the activity log, and support for Windows, physical Fire TVs and other smart TVs.

### The configuration website

The Carbon signs in at `bridge.teamofsilicons.com` with Silicon IAm and picks the org. A Carbon only sees the devices they have paired. The paired devices can either be personal (only the carbon who set it up can see, or it can be organisation wide anyone from the organisation can view this device has been configured - by default each device would be organisation wide.)

**Devices:** a list of every device the Carbon has paired, with its name, its OS, whether it is online, which Silicon is using it right now, when it was last used, and how many days are left before the pair ends.

**Add a device:** the Carbon picks the kind of device and gets a step by step guide for it:

1. Download the right Bridge app, with the link for that device.
2. Open the app, which shows a pairing code.
3. Enter the pairing code on the website.
4. Name the device.
5. Finish the device's own setup (turning on debugging, allowing permissions), with each step explained.
6. Choose which Silicons get access. This can be skipped and done later.

**A device's page:**

- rename the device
- see which Silicons have access, give access to more, or take it away
- see which Silicon is using it right now and since when, and stop it
- set how long the device stays paired without activity, from 1 to 30 days (14 by default)
- see the activity log: every action, which Silicon did it, and when
- see the requests Silicons have sent each other for this device, with their reasons
- remove the device (dangerous action - prompt for confirmation)

### The Bridge app

Every Bridge app works the same way, whatever the device:

1. **Before pairing** it shows a pairing code, and never asks the Carbon to log in.
2. **During setup** it walks the Carbon through what that device needs, one step at a time.
3. **Once paired** it shows the device's name, the Carbon it is paired to, and whether a Silicon is using it right now.
4. **While a Silicon is using the device** it shows which Silicon it is, with a button to stop it.
5. **Revoke pair** is the only other option. It removes the device from the Carbon's account and ends every Silicon's access to it. (Danger action, prompt for confirmation)

The app starts on its own when the device starts, and keeps the device connected to Bridge.

Devices that can't run the Bridge app (iPhone, iPad, Apple TV and other smart TVs) are paired through a Mac or computer the Carbon has already paired. On those devices, the indicator and the stop button are on that computer and on the website.

### Pairing

The pairing code is what ties a device to a Carbon. It is short, it works once, and it expires after a few minutes. Pairing code is a 6 digit hexanumerical code, which keeps rotating every 5 minutes. After pairing, the device stays paired until:

- the Carbon revokes the pair on the device,
- the Carbon removes the device on the website, or
- the device goes without activity for as long as the Carbon has set (14 days by default, anywhere from 1 to 30 days).

### Access

A device belongs to the Carbon who paired it. The Carbon gives access to one or more Silicons and can take it away at any time. Removing access, removing the device, revoking the pair, logging out, or leaving the org ends it immediately, even in the middle of a session.

### How a Silicon uses a device

A Silicon uses Bridge through the `bridge` CLI. `bridge` wraps agent-device: every agent-device command is a valid `bridge` command.

1. `bridge device ls` lists every device the Silicon has access to, with its OS, whether it is online, and which Silicon is using it.
2. `bridge device show {deviceid}` shows what the Silicon can do on that device, based on its OS.
3. `bridge session new {deviceid}` starts using the device and prints a session id.
4. `bridge session connect {sessionid}` connects the Silicon to that session.
5. Once connected, the Silicon runs agent-device commands directly, like `bridge snapshot` or `bridge click @e2`.
6. `bridge session end {sessionid}` finishes, and frees the device for other Silicons.

A session id is 3 hexadecimal characters, like `a3f`. Once every 3 character id has been used, session ids grow to 4 characters, and so on.

While connected, `bridge --help` shows the commands that work on that device and nothing else. So a Silicon on a TV sees remote buttons, a Silicon on an Android phone sees Android debugging, and a Silicon on a computer sees the terminal.

A session also ends on its own after 5 minutes without any action, so a Silicon that forgets to end a session doesn't hold the device.

### One Silicon at a time

Only one Silicon can use a device at a time. When a device is in use, any other Silicon with access sees which Silicon is using it, and can send that Silicon a request to use the device with `bridge request send {deviceid} --reason "..."`.

The request carries the reason for use, up to 300 characters. Bridge delivers it to the Silicon using the device as a notification through Ting ([Ting docs](https://ting.teamofsilicons.com/#docs)), with the reason exactly as it was sent.

### Always visible

While a Silicon is using a device, the device shows which Silicon it is, and the Carbon can stop it with one tap, on the device or on the website. Stopping ends the session straight away.

Every action is logged, and the Carbon can see the log for their own devices on the website.

### Files

Every file a session makes, like screenshots, recordings and saved logs, is stored in Briefcase ([Briefcase docs](https://docs.briefcase.teamofsilicons.com/)), in the private folder of the Silicon that made it. Bridge stores it on the Silicon's behalf through Briefcase's OBO endpoint, and automatically shares it with the Carbon who owns the device with create, read and update access (not delete), so the Carbon can see and work with every file made on their devices.

Every file self destructs 1 day after it is stored. If the Silicon wants a file to last longer, it can set a longer self destruct when the file is made (up to 30 days), or make the file permanent before it self destructs.

The `bridge` CLI gives the Silicon the Briefcase link to each file it makes.

# Devices

### Android phones and tablets

**The app:** Silicon Bridge for Android, downloaded from `bridge.teamofsilicons.com`.

**Setup:**

1. Install and open the app, and enter the pairing code on the website.
2. Turn on Developer options. The app shows exactly where.
3. Turn on wireless debugging, and let the app pair with it.
4. Allow the app to show notifications and to stay running in the background.

**How it works:** the app keeps the device connected to Bridge. When a Silicon sends a command, the app carries it out on the device through Android debugging. The first time a Silicon uses the device, agent-device's small helper apps for reading the screen and typing are installed on it.

**While in use:** a notification says which Silicon is using the device, with a Stop button.

**A Silicon can:** open any app, see the screen, tap, type, scroll, swipe, press back, home and recent apps, read notifications, install apps, take screenshots and recordings, read device logs, and use Android debugging.

**Good to know:** the device needs to be on Wi-Fi (any network) for a Silicon to use it. After the device restarts, wireless debugging turns off, and the app asks the Carbon to turn it back on.

### Android TV and Google TV

This also covers Fire TVs that run Fire OS.

**The app:** Silicon Bridge TV.

**Setup:**

1. Install and open the app. It shows the pairing code large on the TV. Enter it on the website.
2. Turn on Developer options and network debugging. The app shows exactly where.
3. Approve the "Allow debugging" prompt on the TV.

**How it works:** the same as Android phones. The TV connects to Bridge by itself, so nothing else is needed at home.

**Showing things on the TV:** the app has a full screen display. A Silicon can put a link, an image, a video or text on it. It stays on screen until the Silicon clears it or someone presses back on the remote.

**While in use:** a small badge in the corner of the screen says which Silicon is using the TV. It can be stopped from the Bridge TV app or from the website.

**A Silicon can:** open any app, press any remote button, see the screen, install apps, take screenshots, read device logs, use Android debugging, and show anything on the screen.

### Mac

**The app:** Silicon Bridge for Mac, which lives in the menu bar.

**Setup:**

1. Install and open the app, and enter the pairing code on the website.
2. Allow Accessibility and Screen Recording when asked. The app opens the right settings page.

**How it works:** the app keeps the Mac connected to Bridge and carries out a Silicon's commands on the Mac.

**While in use:** the menu bar icon changes and a banner says which Silicon is using the Mac, with a Stop button in the menu.

**A Silicon can:** open any app, see and use any window, menus and the menu bar, click, type, scroll, use the clipboard, take screenshots and recordings, and use the terminal.

**Good to know:** a Mac can't be used while it is locked or asleep. The Silicon uses the real mouse and keyboard, so the Carbon and the Silicon can't both use the Mac at the same moment. A paired Mac is also what iPhones, iPads and Apple TVs pair through.

### Windows

**The app:** Silicon Bridge for Windows, which lives in the system tray. agent-device doesn't support Windows, so Bridge builds this itself.

**Setup:**

1. Install and open the app, and enter the pairing code on the website.
2. Allow it when Windows asks.

**How it works:** the same as the Mac.

**While in use:** the tray icon changes and a banner says which Silicon is using the computer, with a Stop button.

**A Silicon can:** open any app, see and use any window, click, type, scroll, use the clipboard, take screenshots and recordings, and use the terminal.

**Good to know:** a Windows computer can't be used while it is locked. Admin prompts always need the Carbon. Like the Mac, the Silicon uses the real mouse and keyboard.

### Linux

**The app:** Silicon Bridge for Linux.

**Setup:**

1. Install and open the app, and enter the pairing code on the website.
2. On newer desktops, approve screen sharing and remote control once when asked.

**How it works:** the same as the Mac.

**While in use:** a banner says which Silicon is using the computer, with a Stop button.

**A Silicon can:** open any app, see and use any window, click, type, scroll, take screenshots and recordings, and use the terminal.

**Good to know:** a Linux computer can't be used while it is locked. A computer without a screen, like a server, only gets the terminal.

### iPhone and iPad

**The app:** there is no Bridge app to install from the iPhone. Bridge puts a small helper on the iPhone through the Carbon's paired Mac.

**Setup:**

1. On the website, choose to add an iPhone or iPad, and pick the paired Mac it will connect through.
2. Plug the iPhone into that Mac once, and tap Trust on the iPhone.
3. Turn on Developer Mode on the iPhone (Settings, then Privacy & Security). The iPhone restarts.
4. Bridge puts its helper on the iPhone. After that, the cable isn't needed as long as the iPhone and the Mac are on the same Wi-Fi.

**How it works:** a Silicon's commands reach the Mac through Bridge, and the Mac carries them out on the iPhone through the helper.

**While in use:** the Mac's Bridge app and the website show which Silicon is using the iPhone, with a Stop button.

**A Silicon can:** open any app, see the screen, tap, type, swipe, scroll, use any app, and take screenshots and recordings.

**Good to know:** the iPhone must be near its Mac (same Wi-Fi or plugged in), awake and unlocked. There is no Android style debugging, and a Silicon can't approve payments or Face ID. Revoking the pair is done from the Mac's Bridge app or the website.

### Apple TV

**Setup:** on the website, choose to add an Apple TV and pick a paired Mac on the same network. Enter the code the Apple TV shows.

**How it works:** the Mac carries out a Silicon's commands on the Apple TV over the home network.

**A Silicon can:** open apps, press remote buttons, and show pictures and videos on the screen.

**Good to know:** the Apple TV must stay on the same network as its Mac.

### Other smart TVs (Samsung, LG)

**Setup:** on the website, choose to add the TV and pick a paired computer on the same network. Approve the connection on the TV.

**How it works:** the computer carries out a Silicon's commands on the TV over the home network.

**A Silicon can:** open apps, press remote buttons, and open links.

**Good to know:** a Silicon can't see what is on these TVs' screens. The TV must stay on the same network as its computer.

# Versioning

For versioning we have Contract Governance/API/service contract lifecycle management. We will have:

1) Contract versioning / API versioning
2) Protocol Negotiation
3) Backward compatibility
4) Consumer-driven contract testing
5) Deprecation and sunset management - if 0 requests for 7 days, sunset that version
6) Compatibility matrix
7) Version policy

# Testing Environment

For testing we would use the environments managed by Honeycomb. IAM would still handle the test identities, login and authorization, and Bridge would handle its own test devices, pairings, access and sessions.

A test environment is basically the same Bridge where I can test pairing a device, giving and taking away access, starting and ending sessions, sending requests, and checking what each user is allowed to do. It starts empty and uses the same APIs and workflows with completely isolated data.

### Environment Lifecycle

Bridge would accept authenticated instructions from Honeycomb to prepare the environment, update its key version, clean, disable, restore and permanently remove its test data. Keep the same environment_id across the services. Each operation must be safe to retry and report whether Bridge's work is pending, completed or failed. These instructions must work even when the test sessions have been disabled.

For creation and restoration, Bridge would only allow test access once Honeycomb confirms all required services are ready. If IAM enforces this shared readiness, Bridge must check that current IAM state before allowing access. Finishing its own preparation alone does not make the environment ready.

Cleaning keeps the environment but clears its paired devices, access, sessions, requests, activity logs and other test data, and unpairs every device paired into it. Block access while cleaning and check the environment revision and cleaning generation so old requests, jobs or webhooks cannot bring back cleared data. Only report completion once the cleanup has finished. Cleaning must keep Bridge linked to the environment, so later deletion, restoration and permanent removal still reach it.

Honeycomb decides inactivity expiry and the recovery period. Bridge reports activity and carries out the cleanup instead of independently retiring the shared environment. Disabling blocks access immediately and ends every running session, restoring makes retained data available again when authorized, and permanent removal clears everything that remains. Restoring cannot undo a clean.

### Using a Test Environment

In the Bridge apps, website, CLI, or API, passing the test environment's `app_secret` would select that application's test environment. No manual pairing or separately entering the environment root key should be needed. Bridge should validate the secret with IAM and identify the correct environment automatically.

For logging in, it would ask for an SLT. In a test environment, this can either be an IAM-issued test SLT or the public ID of an existing Carbon/Silicon in the test sandbox. Entering the ID would sign me in as that test user. Unknown or inactive identities should be rejected. This shortcut must never work in production.

The testing_key managed by Honeycomb gives administrative control over the test world. The application's `app_secret` selects its sandbox. Once signed in as a particular user, actions must follow that user's actual permissions. Possessing the secret must not make every signed-in user bypass permission checks.

If an administrative or god view is provided, it should be separate and clearly labelled so it cannot be confused with testing what a normal user is allowed to do.

### Devices in a Test Environment

A device is paired into a test environment the same way as in production: the Carbon enters the device's pairing code on the website while in that test environment. A device paired into a test environment exists only there. A device can be paired to only one environment at a time, production or test.

While a device is paired into a test environment, its Bridge app always shows a banner saying so, with the environment's name.

### Website and CLI

Environment creation and management from the website, CLI or client would go through Honeycomb. Bridge would still let me enter an existing environment and use its normal device and session commands.

On the website, I should be able to enter the `app_secret` from settings or the sign-in screen. Without a selected test environment, the application would use production.

When in a test environment, always show a banner at the top saying that I am currently in a test environment, along with its name, the signed-in test identity, and a button to exit testing mode.

Production and testing sessions should remain separate. Exiting testing mode should return me to the production session or ask me to sign in.

In the CLI, always display the selected test environment at the end, including when a command fails. This message should go to stderr so it does not interfere with JSON output, screenshots, recordings, or commands used in scripts.

### Isolation

Everything belonging to a test environment must stay inside that environment, including paired devices, pairing codes, access, sessions, session ids, requests, activity logs, screenshots, recordings, notifications, background jobs, and audit logs.

Production credentials must not work in testing, and credentials from one test environment must not work in another.

If a supplied test secret is invalid, revoked, or belongs to an unavailable environment, return an error. Never silently continue in production.

### Test Environment Limits

Each Bridge test environment would have a maximum of **5 paired devices**. If pairing one more device would exceed it, return:

`In test environment you are limited to 5 paired devices per environment.`

There would be a maximum of **10 simultaneous active test environments across the Silicon Bridge deployment**. Retired environments would not count as active, and restoring one would require an available slot.

### Webhooks and External Actions

Test webhooks should follow IAM's documented format. Verify the signature over the complete raw body, identify the correct test environment, and apply the event only there. Duplicate or out-of-order events must not corrupt the current state.

Test actions should not send real emails, SMS messages, payments, or other production effects. These should use test destinations or simulated delivery. Requests between Silicons in a test environment go through Ting's test environment, never production Ting. Files made in a test environment are stored in Briefcase's test environment, never production Briefcase.

Secrets must not appear in URLs, logs, audit records, or stored webhook payloads.

### Pairing Codes

Pairing should behave like production. A pairing code made in a test environment pairs a device only into that test environment.

---
---
---
---
---
---
---
---
---
---
---
---
---

Only above this line is what the Bridge backend would hold, below this would be the users of the backend: the configuration website, the Bridge apps, the cli, etc.

# Rust Package & CLI

The Rust package & cli using that rust package are first hand client with an always running deamon if needed in the background. the UI will be a subset of the cli. make sure everything works via the CLI first, and then we'll make the UI. Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, read, write, patch, delete, everything).

The rust package would be stateless whereas the cli would be statefull. CLI built on top of the rust package.

For how this CLI is built, rust as the programming language, but can use anything under the hood that is needed. Maybe rust, or node, or shell, as and when the work comes. That is decided by the implementor based on the work. If something requirs a UI (like graph, live, video, images etc). for that the UI has an endpoint that can be viewed/used/downloaded and the cli gives the link to that.

The primary Interface is the Rust Package. CLI is built using the Rust Package only and doesn't have any feature that the Rust package does not.

if you need a local store for auth or something else, use `{home_dir}/.{appname}/dir`.

The default home dir is `~`. If `SILICON_HOME` is present in the enviorment variables, use that as the home directory by default.

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package.

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package.

Testing enviorment in cli, for testing enviorment in cli i should just be able to `bridge --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token.

For CLI login there should be this exact command: `bridge login <slt>`.
And there should be an command to configure the home directory where the information is stored: `{home_dir}/.{appname}/dir`. This can be confitgure via `bridge config home {location}`. If it's not a directory give an error not a directory.

It should also expose these specific commands:
1) `--help` which would give all the help documentation on how to use bridge. So the user should be able to run `bridge --help` and get the help docs.
2) `iam --json` the user should be able to run `bridge iam --json` which returns `app_id` alongside other information.
3) `login status --json` the user should be able to run `bridge login status --json`, reports successful authentication reports `authenticated: true`, alongside which carbon or silicon is it authenticated as.


# Cli experience

CLI is the primary way to interact with IAM Apps. It should be built for both Carbons & Silicons. Any other interface (like website) will be a subset of the CLI.

The cli should never ask for credentials from either silicon or carbon. it should just ask for short lived tokens that the user can generate from the official iam cli, or from the web where the the user is sent to auth concent screen.

CLIs get SILICON_HOME env variable where it should store all the details. Its home, so you should use that as base, and make their own hidden folders to keep their information.

Specific apps that could benefit from using ISI env variable should do that. eg: dm.

ISI are internal silicons. If silicon is a brain, then isi are parts of the brain. store this inside metadata, or main data if its super useful. ISI may or may not be present. make sure to not rely on it in such a way that things break. consider ISI as useful additional information.

every app cli must support the following commands:

`app iam --json` gives {app_id: "...", ...}

`app login "..."` takes in a short lived auth token generated by silicon interpretter.

`app login status --json` tells if its {authenticated: true, ...}


App Internals:
All apps are suggested to make a rust library which is stateless. then 2 things that uses the rust library: always running daemon, and a cli interface that talks to the daemon.

On the docs page, show `honeycomb install 'bridge'` to install the CLI, followed by how to log in.

CLI design should be focused on giving details and helping finding the right command to use. CLI will often have lots of commands and it should be like a tree that can be traversed using --help.

CLI documentation should be bundled inside the cli itself. On each print of the cli documentation using --help or otherwise, it should show what this command is for, how its often used (perhaps in conjunction with other commands if applicable) and then a list of flags etc it takes in.

Follow the CLI grammar. These CLIs can be used by humans, but more often than not, it'll be used by an agent who prefers to know why something broke and so it can figure out ways to fix it. Don't just say something went wrong... tell it exactly what & why.

A good rule of thumb is: these CLIs are being made for someone who understands ins-and-outs of technology. Make like a programming language that gives very specific and helpful errors and outputs compared to a web interface where all errors are hidden until absolutely critical.

All CLIs must have a report bug feature that also optionally takes in a PR ref if the agent did not just find a bug but also patched it.

bridge report `<report-message>` --pr `<pr-link>` and if someone just reports the bug, without the pr, show them a message, you can also put a pr in the repo (`repo-link`).

Everytime a bug is reported use postmark to mail [saketdev12@gmail.com, shubhastro2@gmails.com, bugs@teamofsilicons.com]

Since all TOS applications are open sourced, any bug can be discovered, replicated, patched and a pr can be raised. Allow all such edge cases be figured out by the agent instead of fixing it ourselves based on a bug report.

Only a bug report submitting is possible, but its encouraged to give a lot more details and also attach a PR if possible.

Give the information of the github repo, online docs, rust package, etc inside the cli itself.

The CLI as i told before is a tree of documentation. Show possible paths, and then let someone go deeper along with documentation.


# Docs

There are two kinds of documentations: informative & instructive.

Always keep instructive documentation up front, easy to use, direct with clear instructions & link to informative documents to know why its done this way. Instructive documents should be the landing point of the product for both carbons & silicons.

It can give carbon the instructions on how to install & use it, or how to ask their silicon to use it.

For silicons, it can be that, but also how to do a lot more with it. Esp. things like building on top of it. Make it very clear what is expected, what is mandatory and how does the system work.

Then the silicon can dig deeper into the informative documentation to know all the possible ways to do it, & why its done the way its done.

While both carbons and silicons can read the documentation, it'll likely be more silicon. So design it for silicons. The more reasons you give, the better a silicon would be at making a judgement call of how to do something.

Since all IAM apps can both be used as is, and also built on top of... its imp to write documentation for both. Usage docs & Development docs.

# Telemetry

All IAM apps use Space Station [https://spacestation.teamofsilicons.com/docs] for telemetry. Telemetry is opted-in by default but can be opted out from settings if the user wants.

Space Station is also a rust package which can be used from within the backend, or daemon, or cli to send telemetry.

Record as many things as you think might be useful to diagnose or follow traces later.

Since space station is just an event store, make sure to include all the source, step, progress, etc information inside each event. some of the system information is automatically added to the metadata so you need not add that.

push context-rich, self-contained events.

Space Station also support web, for web it has 2 possible pathways: analytics & events. Most of the Analytics is self captured and you can define a seperate event store from the web. Its possible that both web analytics and web events go to separate tables.


# Configurability

We ship highly configurable apps with sensible defaults. Very much like VS Code. flags to toggle / customize behaviors.
