# Interface design and visual testing

## Design basis

This applies Material 3 to a desktop infrastructure app. Google's
[Expressive research](https://design.google/library/expressive-material-design-google-research)
uses color, size, shape and containment to guide attention and group related
content, while warning against replacing familiar interactions or removing labels.
Here that means tonal surfaces, rounded cards, clear primary actions and labeled
navigation. Expressive springs and shape feedback come from iced-m3's theme;
the Host page also exposes a reduced-motion option.

Related primary guidance:

- [Material typography roles](https://m3.material.io/styles/typography/applying-type)
  distinguish page headlines, section titles, body copy and labels.
- [Material color roles](https://m3.material.io/styles/color/the-color-system)
  separate surfaces and containers from high-emphasis actions and error states.
- [Material navigation rail](https://github.com/material-components/material-components-android/blob/master/docs/components/NavigationRail.md)
  keeps labeled top-level destinations available on wider windows.
- [Material common buttons](https://github.com/material-components/material-components-android/blob/master/docs/components/CommonButton.md)
  provide different emphasis levels instead of filling every action.

## Structure

Workspaces is the default destination: resource summaries, VM cards, terminal
access, T3 setup and a contextual Manage dialog. Create and resize use separate,
target-specific forms. GPU assignment and destructive actions sit in Manage;
permanent deletion requires its own confirmation.

Host contains pool health and hardware readiness. Technical evidence and stable
identifiers are available through disclosure controls. People contains user grants,
invitations and device revocation; ordinary users do not get this destination.
Activity presents requests by action and machine name with visible progress and
an explicit administrator retry for operations requiring review.

The narrow layout condenses resource summaries and uses one VM column. Wider
windows use two columns. Content reserves a scrollbar gutter; forms keep their
actions visible when scrolling. Invitations remain hidden from screenshots.

## Run screenshot tests

```sh
sh scripts/test_ui.sh
```

These tests render the **same native iced view tree** as the app through
`iced_test::Simulator`. They use the software tiny-skia renderer, bundled Roboto,
fixed fixture data, and reduced motion. They never load a real profile, connect to
Iroh, enroll a device, or mutate the Linux host. They cover 17 screens/states,
including 800×600, 1180×820 and 1440×900 windows, a member account, empty and
over-quota states, hardware setup, and the principal dialogs.

Baselines live in `crates/desktop/tests/screenshots/`. Every run writes fresh
actual images to `target/ui-screenshots/`. A changed pixel fails the test and
produces a highlighted `*-diff.png`. Missing baselines fail too; a normal test
run cannot silently approve new output. The macOS CI job runs the comparisons
and uploads failure artifacts.

For an intentional visual change:

```sh
UPDATE_SCREENSHOTS=1 sh scripts/test_ui.sh
```

Open the generated images and inspect layout, clipping, hierarchy, contrast,
button labels and dialog footers before accepting the baseline changes. Then run
the command again **without** the update flag. Exact pixel comparisons are pinned
to this renderer/font combination; investigate platform rasterization differences
before treating a CI mismatch as a design change.

Interaction tests also exercise navigation, dialog cancellation, resource prefills,
member navigation permissions, and target-specific management actions. Pixel
tests do not replace native macOS checks for window chrome, clipboard integration,
Terminal automation, VoiceOver or OS-level accessibility behavior.
