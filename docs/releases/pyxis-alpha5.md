# Pyxis alpha 5

Alpha 5 is the release where Pyxis starts to look and feel like its own desktop. It has a new wallpaper that follows the sun, frosted windows with their own shape, a lock screen, and motion that makes opening, minimizing and closing feel connected. Under the surface, every app now runs in its own space, and one Pyxis dialog is the only place that asks for your password.

## Highlights

- **Daylight wallpaper.** Soft colour drifts across the screen and the palette follows the sun where you are, from night blue through dawn, golden hour and full day.
- **A new desktop layout.** A thin status strip sits on the wallpaper, and a launch bar holds your apps as droplet-shaped icons. Move the bar to the left edge if you prefer.
- **Just type.** Start typing on the desktop and search opens. It finds apps, notes, contacts and tasks, and can turn what you typed into an action.
- **Frosted glass windows.** Every window has a title bar of blurred wallpaper, round corners with one sharp corner (the waterdrop shape), and soft shadows that show which window is in front.
- **Cards and Alt+Tab.** See every open window as a live picture, or hold Alt and press Tab to walk through them.
- **Lumen motion.** Windows come into focus as they open, round into a droplet as they minimize, and burn away as they close. You can turn all of it down or off.
- **A lock screen.** Lock with Super+L or let Pyxis lock itself when you step away.
- **One password dialog.** Installing apps, changing the network and managing users all ask in the same Pyxis dialog, which says who is asking and why.

## The desktop

- The status strip shows search, the time and your Wi-Fi, volume and battery. Its text changes colour with the wallpaper so it stays easy to read.
- The launch bar shows which apps are running, starting, minimized or crashed with a thin light under each icon.
- The app drawer lists every app. New apps appear as soon as they are installed.
- Quick settings gives you Wi-Fi, Do Not Disturb, dark theme, volume and brightness, plus Lock, Sleep, Restart and Shut down.
- Notifications rise in the bottom-right corner above your windows, then wait in the notification centre. Do Not Disturb keeps them quiet without losing any.
- Pyxis has its own system sounds for starting up, notifications, errors and shutting down.

## Windows and motion

- Windows never slide under the strip or the launch bar. Maximized windows fill the space between them.
- If an app stops responding, the window controls start to move so you can tell it is busy.
- The controls use the shapes and colours of the Pyxis spinner.
- Snap windows with Super and the arrow keys. Super+Q closes a window. Super+D shows the desktop, and pressing it again brings the windows back in order.
- Maximizing a window ends with a soft band of light across it.
- In Settings, Accessibility, choose Full, Reduced or Off. Reduced keeps only short fades. Off makes every change instant.

## Making it yours

- Pick Light, Dark or Auto. Auto turns dark at dusk and light at dawn.
- Choose Daylight, Aurora, Sunset, Forest or Mochi. Let it flow, hold it still, or pin one time of day.
- The shell takes its colours from your wallpaper, so they always match.
- Settings changes show up at once in every app, with no restart.
- Date & Time lets you search time zones by city, switch between 12 and 24 hours and see today's sunrise and sunset. Daylight uses your place to follow the sun.

## Lock screen and security

- The lock screen shows the clock and nothing else until you start typing. After five wrong passwords you wait, a little longer each time. If your account has no password, the lock opens with a press.
- Pyxis locks after a time you choose (10 minutes to begin with) and when the computer sleeps. You choose whether the lock screen shows nothing, a count or the full text of notifications.
- Installing or removing an app, changing the network and adding or removing users all ask for your password. Restart and Shut down ask you to confirm.
- Each app now runs as its own user, with its own files and only the permissions it declared.
- Apps cannot read each other's data, watch your screen or type into other windows.
- Apps install only from signed packages. Development images also allow unsigned ones.
- A firewall blocks unexpected connections from outside.

## Apps

All eight apps are rebuilt on Duyo, the new interface toolkit.

- **Files**, **Notes**, **Contacts** and **Tasks**. Notes, contacts and tasks show up in desktop search.
- **Settings** has a page for each of Appearance, Sound, Accessibility, Network, Date & Time, Users & Lock, System and About.
- **Vela**, the web browser, has a new start page with the time, a greeting, one box to search or type an address, and your pinned sites. Every new tab starts there.
- **Terminal**, now running as you and not as the system administrator.
- **App Installer** shows what an app is and what it wants to do before it installs.
- Shortcuts such as Ctrl+N now go to the app you are using. Before, Ctrl+N in Notes opened a Firefox window.

## Under the hood

- Pyxis draws all window frames itself, so every window looks and behaves the same.
- Apps are kept apart by the system, and the shell is the only thing that can see the whole screen.
- The kernel image gains stricter security settings, and apps get working sound.
- A test suite that drives a running Pyxis through clicks, keys and screenshots checked these features on a real boot.

## Known issues

- There is no update mechanism yet. To move to a new version, install a new image.
- If the shell's process is killed, it does not come back on its own. We have not found why. Restart the VM.
- Native apps are not yet confined beyond running as separate users, so a native app is not fully sandboxed.
- A window's title bar frosts the wallpaper only. Over another window it shows the wallpaper, not that window.
- The firewall and kernel hardening need the newly built image and kernel. They have not yet been checked on a device.
- The Wi-Fi password is briefly visible to other programs while you connect.
- Some apps still draw their buttons with brackets, such as "(OK)".

## Trying it

Pyxis boots as a disk image in a virtual machine. Give the VM at least 4 GB of memory if you want to run several apps at once. For a tour with screenshots, open `docs/feature-guide/index.html`.
