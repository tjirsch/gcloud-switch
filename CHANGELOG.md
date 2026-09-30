# Changelog

The notable changes of each release, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) form. cargo-dist reads the section of the released version into the GitHub release notes. Releases before 0.11.0 are listed only on the [Releases](https://github.com/tjirsch/gcloud-switch/releases) page.

## [0.11.0] - 2026-09-30

### Added

- `gcloud-switch status`, and two lines above the TUI's table, show what gcloud holds right now: the active configuration (name, account, project) and the live ADC file (profile, account, quota project), each with the validity of its credential and a note when it is not what the profile says.
- A profile with one account for both parts gets its ADC without a browser when the user credential is valid: the ADC is derived from that credential, the document `gcloud auth login --update-adc` writes. When both parts need a login, one browser round with `--update-adc` serves both.
- Activation says what it set: `Activated profile 'x': project p, ADC quota project q.`; an ADC login says what it stored and for which account.

### Changed

- The ADC login is given the profile's ADC account again, so gcloud verifies that the browser signed in as that account and refuses otherwise; the live ADC file is moved aside during the login so gcloud never skips it, and put back when the login fails. Every stored ADC credential names its account.
- Both logins run with `--verbosity=error`: gcloud's "Quota project is disabled" warning described the file a moment before the quota project was stamped in.
- The profile whose ADC is live is matched by refresh token and quota project, preferring the active profile, since profiles with one account can share a token.

### Fixed

- A typo in a profile's ADC account was accepted, because the check compared it with an account field gcloud had left empty. It is now refused by gcloud at the next login, naming both accounts.
