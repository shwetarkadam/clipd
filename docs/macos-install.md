# Installing clipd on macOS

macOS shows a warning for apps it cannot verify. clipd is code-signed, but not
*notarized* — notarization requires a paid Apple Developer Program membership,
and clipd is free and open source.

The warning is about paperwork with Apple, not about the app. Here is how to
avoid it, and how to get past it if you have already hit it.

## Recommended: one line, no warning

```
curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
```

This puts **Clipd.app** in Applications and the `clipd` command in your path,
and **no security warning appears**.

Not a trick: the warning is triggered by a flag (`com.apple.quarantine`) that
your *browser* attaches to anything it downloads. `curl` does not attach it, so
there is nothing for Gatekeeper to object to.

## If you already downloaded the .zip

You will see:

> **"Apple could not verify Clipd.app is free of malware that may harm your Mac."**

Any one of these fixes it.

**Easiest — run the installer over it.** The command above replaces the copy
and clears the flag:

```
curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
```

**Or open it once by hand.** Try to open Clipd, dismiss the warning, then:

1. **System Settings → Privacy & Security**
2. Scroll to **Security**
3. Next to *"Clipd.app was blocked"*, click **Open Anyway**
4. Confirm with Touch ID or your password

On macOS 15 and later the warning dialog has only a **Done** button, and the
old right-click → Open shortcut no longer works. System Settings is the way.

**Or clear the flag yourself**, if you prefer the terminal:

```
xattr -dr com.apple.quarantine /Applications/Clipd.app
```

## After installing

macOS asks for two permissions. clipd needs both, and asks for nothing else:

- **Accessibility** — pasting into the app you were using
- **Input Monitoring** — multi-slot copy counts your ⌘C presses, which means
  reading the keyboard

If you granted them and multi-slot still does nothing, **restart clipd**. macOS
hands a new permission only to a process that starts after it is granted, so a
running copy keeps the old answer. The same is true after every update: the
grant is tied to the exact binary, and an updated clipd is a new one.

## Why not Homebrew?

Homebrew removed support for casks that fail Gatekeeper checks on 1 September
2026. An app that is not notarized cannot be published there, so there is no
`brew install clipd` and there will not be one until clipd is notarized.

## Is this safe?

Judge it for yourself rather than taking a claim on faith:

- The source is public, including the installer that runs in the command above:
  <https://github.com/shwetarkadam/clipd/blob/main/install.sh>
- Every release is built by GitHub Actions from a tagged commit, in the open —
  the binary you download is assembled by the workflow in the repository, not
  uploaded from anyone's laptop.
- The app is code-signed, so macOS can confirm it has not been altered since it
  was built. Notarization is the extra step of sending it to Apple for an
  automated malware scan, which costs $99/year.
