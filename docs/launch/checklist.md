# Go-public checklist

Status: ready, nothing done yet. Written 27 November 2026 by the webmaster lane. Do the steps in
order. Nothing here has been posted, and no one has been contacted.

Venue facts (open or closed, last activity) were checked on 27 November 2026. Check each one again
on launch day, and do not post in a thread that has closed or gone quiet for years.

## 1. Make the repo public

1. GitHub > `lelanddutcher/dat-file-repair` > Settings > General > Danger Zone > Change visibility
   > Public. Read the tree first: no media, no session notes, no credentials (see `AGENTS.md`).
2. Settings > General > Social preview: upload `assets/brand/social_preview_1280x640.png`.
3. Set the description, homepage and topics. The bot got HTTP 403 on 27 November 2026, so paste
   them by hand (the gear icon next to **About**), or run as the owner:

   ```sh
   gh repo edit lelanddutcher/dat-file-repair \
     --description "Repair the .DAT file a Canon camera leaves when a recording is cut off (battery died, card removed, Err 70). DATfix rebuilds the MP4 index with no re-encoding and uploads nothing. Verified on EOS R5 Mark II, R5 and R3. Can also recover a 0-byte or missing .DAT from a card image. Free and open source (MIT) for Mac, Windows and the command line." \
     --homepage https://lelanddutcher.github.io/dat-file-repair/ \
     --add-topic canon,canon-eos,canon-r5,canon-r5-mark-ii,canon-r3,dat-file,video-repair,mp4-repair,video-recovery,corrupted-video,data-recovery,file-recovery,mp4,hevc,untrunc,rust,tauri,cli,macos,windows
   ```

   The description is 345 characters (the limit is 350). 20 topics (the limit is 20).

4. Check: `gh api repos/lelanddutcher/dat-file-repair --jq '.private, .description, .topics'`.

## 2. Enable GitHub Pages

The site is `docs/index.html` with its files in `docs/site/`. `docs/.nojekyll` turns Jekyll off,
so the Markdown in `docs/` is served as plain files and cannot break the build.

1. Settings > Pages > Source: **Deploy from a branch**. Branch `main`, folder `/docs`. Save.
2. Wait for the deployment, then open https://lelanddutcher.github.io/dat-file-repair/.
3. Check:
   - `curl -sI https://lelanddutcher.github.io/dat-file-repair/` returns 200.
   - The OG image loads: https://lelanddutcher.github.io/dat-file-repair/site/og_1200x630.png
   - The page's FAQ and app data pass https://search.google.com/test/rich-results (paste the URL).
   - On a Mac the button says "Download for Mac". On Windows it says "Download for Windows". Before
     the first release both go to the releases page.

## 3. Submit the site to search engines

1. Google Search Console: add a **URL-prefix** property for
   `https://lelanddutcher.github.io/dat-file-repair/`. Verify with the HTML-tag method: add the
   `<meta name="google-site-verification">` tag to `docs/index.html` in a PR. (A domain property is
   not possible on `github.io`.) Then URL Inspection > Request indexing for the page.
2. Bing Webmaster Tools: Add site > import from Search Console, or add the same URL and verify with
   the `msvalidate.01` meta tag. Then URL Submission for the page.
3. Add the page to the projects list on lelanddutcher.com (portfolio lane), linking with the text
   "DATfix, free Canon .DAT file repair".

## 4. Make the first public release

1. Bump the version in `Cargo.toml` (`[workspace.package]`) and `app/package.json`, then push tag
   `v<version>`. `.github/workflows/release.yml` builds the files and makes a **draft** release.
2. Check the draft has: `DATfix_<v>_macos_universal.dmg`, `DATfix_<v>_windows_x64-setup.exe`,
   `datfix-cli-<v>-aarch64-apple-darwin.tar.gz`, `datfix-cli-<v>-x86_64-apple-darwin.tar.gz`,
   `datfix-cli-<v>-x86_64-pc-windows-msvc.zip`, `SHA256SUMS.txt`.
3. Publish the draft. Do not mark it as a pre-release: the site's button skips pre-releases.
4. Check: on the site, the Mac button's link ends in `_macos_universal.dmg` and the Windows
   button's link ends in `_windows_x64-setup.exe`.
5. Re-check the competitor prices in `README.md` ("How DATfix compares") on the vendor pages and
   update the date.

## 5. Where to post, in order

Rules for every post: say you made it, say it is free and MIT, name the verified cameras and say
which are only expected. No dashes, no hype. Answer the person's question first. Post only where the
thread is open and the person may still need help. One post per venue.

### 5.1 untrunc issues (GitHub)

Be a good neighbour: untrunc did this first and its author just added Canon support. Post only
where DATfix covers the reporter's camera. Do not post in #303 (R6 Mark II, fixed by untrunc,
closed).

| Issue | Camera | State on 27 Nov 2026 | Post? |
|---|---|---|---|
| https://github.com/anthwlock/untrunc/issues/84 | R5 Mark II (comment of Nov 2025; the issue itself is older, avc1) | open, last activity 9 Dec 2025 | Yes, reply to the R5 Mark II comment |
| https://github.com/anthwlock/untrunc/issues/170 | R5 | open, last activity 30 Jun 2024 | Yes |
| https://github.com/anthwlock/untrunc/issues/215 | R5 C | open, last activity 19 Mar 2025 | Only if R5 C gets verified. It is not in our table today. |

Draft (#84):

> For the R5 Mark II `.DAT` files mentioned here: I wrote a free, MIT-licensed tool for this case,
> DATfix (https://github.com/lelanddutcher/dat-file-repair). It reads the `repi` recovery index for
> the XF-HEVC S layout with 4-channel PCM and the `cndm` metadata track, which is why the generic
> scan misreads these files. It is verified on 15 R5 Mark II `.DAT` files at 4K 119.88p. Thanks to
> untrunc for showing the way; this is meant as a complement for the PCM layouts.

Draft (#170):

> If this is still open for you: DATfix (https://github.com/lelanddutcher/dat-file-repair) is a
> free, MIT-licensed tool that repairs R5 `.DAT` files with PCM audio from Canon's `repi` index.
> It is verified on 10 R5 `.DAT` files (HEVC 4K 59.94p All-I, stereo PCM). R5 modes with AAC audio
> are expected to work but not yet tested on a real `.DAT`.

### 5.2 Reddit

Checked with PullPush (`https://api.pullpush.io/reddit/search/submission/?ids=<id>`), 27 November
2026. PullPush shows the post as archived at the time it was collected; Reddit closes replies on
some subreddits after 6 months. Open each link before posting.

| Post | Camera | Posted | Comments | Fit |
|---|---|---|---|---|
| https://www.reddit.com/r/Cameras/comments/1wwyeup/can_i_recover_a_dat_video_file_from_canon_camera/ | R5, 17 GB `.DAT` | 3 Oct 2026 | 0 | Best: recent, unanswered, verified camera |
| https://www.reddit.com/r/videography/comments/1wwhjyc/ | same post | 3 Oct 2026 | 0 | Skip: removed by automod |
| https://www.reddit.com/r/canon/comments/1l2rbyb/how_to_fix_a_canon_corrupted_video_dat_file_canon/ | R5 Mark II, Err 70 | 3 Jun 2025 | 8 | Good: a how-to thread; add DATfix as a free option |
| https://www.reddit.com/r/canon/comments/1pxw8k2/lost_power_while_using_dc_dummy_battery_for_a/ | R5 timelapse | 28 Dec 2025 | 0 | Unsure: timelapse mode is not tested. Only say "may work" |
| https://www.reddit.com/r/AskADataRecoveryPro/comments/1tpwwde/camera_stopped_while_recording/ | R6 Mark II, 0 KB `.DAT` | 28 May 2026 | 11 | Card scan fits; R6 Mark II is only expected. Say so. |
| https://www.reddit.com/r/AskADataRecoveryPro/comments/1jokntz/fixing_a_dat_file_from_a_canon_camera/ | R6, 32 GB | 1 Apr 2025 | 3 | R6 is only expected; ask for a sample |
| https://www.reddit.com/r/datarecovery/comments/1jmpv8n/help_me_with_a_dat_file_from_a_canon_r6_camera/ | R6, 33 GB | 29 Mar 2025 | 0 | Same as above |
| https://www.reddit.com/r/canon/comments/1jeeah5/anyone_with_experience_recovering_dat_files/ | R5 C | 18 Mar 2025 | 3 | Skip until R5 C is verified |

Draft (r/Cameras 1wwyeup):

> Yes, very likely. The 17 GB `.DAT` holds the video; the camera just never wrote the index. I made a
> free, open-source app for exactly this: DATfix (https://lelanddutcher.github.io/dat-file-repair/).
> It is verified on R5 `.DAT` files with PCM audio. Copy the whole card folder to your computer
> first, then drop the `.DAT` in. It works offline and never changes the original.

Draft (r/canon 1l2rbyb):

> Adding a free option for anyone who finds this later: DATfix
> (https://lelanddutcher.github.io/dat-file-repair/) repairs R5 Mark II `.DAT` files from the
> recovery index Canon writes inside them. No re-encoding, runs offline, MIT license. Verified on
> 15 R5 Mark II `.DAT` files.

New posts, after the release and only once each. Read each subreddit's self-promotion rule first:

- r/canon (https://www.reddit.com/r/canon/): "Free open-source tool to repair the .DAT file a
  Canon camera leaves when recording is cut off (R5 Mark II, R5, R3 verified)". Body: the problem in
  two sentences, the verified table, the ask for samples from R6, R6 Mark II, C70 and C80.
- r/videography (https://www.reddit.com/r/videography/): same, framed for event and wedding
  shooters. Also r/WeddingVideography (https://www.reddit.com/r/WeddingVideography/).
- r/editors (https://www.reddit.com/r/editors/): lead with the editing facts: no re-encoding,
  timecode kept, tested in DaVinci Resolve 21.2, Premiere not tested yet.
- r/datarecovery (https://www.reddit.com/r/datarecovery/): lead with the 0-byte card scan.

### 5.3 DPReview forums

- https://www.dpreview.com/forums/threads/canon-r6-mark-2-dat-file.4774045/ : R6 Mark II, camera
  froze during a wedding, 19 minutes. Started 25 Aug 2024, last reply 26 Mar 2025, open for replies
  (needs an account). R6 Mark II is only expected, so the draft must say so:

> In case this helps you or someone who finds this thread: DATfix
> (https://lelanddutcher.github.io/dat-file-repair/) is a free, open-source app that repairs Canon
> `.DAT` files from the recovery index inside them. It is verified on the R5 Mark II, R5 and R3.
> The R6 Mark II writes the same index, but I have not tested a real R6 Mark II `.DAT` yet. If you
> still have the file, I would like to know whether it works.

### 5.4 Canon Community

Could not verify. On 27 November 2026 every `community.usa.canon.com/t5/...` thread link
redirected to a login page, so the state of these threads is unknown. Log in and check each one:

- https://community.usa.canon.com/t5/x/x/td-p/439135
- https://community.usa.canon.com/t5/x/x/td-p/395783
- https://community.usa.canon.com/t5/x/x/td-p/95506
- https://community.usa.canon.com/t5/EOS-DSLR-Mirrorless-Cameras/battery-died-during-video-shooting/td-p/189727
  (a search snippet says this is a T4i with a 0 KB `.DAT`. The T4i is not supported: skip it.)

Post only in threads about a verified or expected camera. Draft:

> DATfix (https://lelanddutcher.github.io/dat-file-repair/) is a free, open-source app that repairs
> the `.DAT` a Canon camera leaves when recording is cut off. Verified on the EOS R5 Mark II, R5 and
> R3. It is not made by or affiliated with Canon.

### 5.5 Other forums

- EOSHD topic 46746
  (https://www.eoshd.com/comments/topic/46746-canon-eos-r5-so-called-overheat-timer-defeated-by-a-single-screw-in-battery-door/):
  an R5 overheating thread from August 2020. It holds horshack's notes on the `.DAT` layout, which
  we cite. Too old and off-topic to post in. Instead, credit it in a new post if EOSHD allows one.
- Creative COW Canon `.DAT` thread: not found again on 27 November 2026. Search creativecow.net
  for "Canon DAT" before posting.
- Gist coltenkrauter/e77d229f7b4afc7fa347cf0099d69d33: about the Vixia HF G50 camcorder, last
  updated 29 Sep 2023, 0 comments. The HF G50 is not supported. Skip.

### 5.6 Show HN

https://news.ycombinator.com/submit. Title (80 characters or fewer):

> Show HN: DATfix, repair the .DAT a Canon camera leaves when recording is cut off

URL: the GitHub repo. First comment:

> Canon cameras write video into a temporary .DAT and only write the MP4 index when you press stop.
> If the battery dies, the index is never written. Canon does write small "repi" headers before
> each group of samples, so the index can be rebuilt exactly. DATfix walks those headers and writes
> a new moov, copying the media bytes unchanged. Rust core and CLI, Tauri app, MIT. Verified on
> 36 real .DAT files (R5 Mark II, R5, R3). The format notes are in docs/format. Samples from other
> Canon bodies are very welcome.

### 5.7 Awesome lists (pull requests, after the release)

Checked 27 November 2026: all active, not archived. Read each `CONTRIBUTING.md` first.

| List | Section to propose | Entry |
|---|---|---|
| https://github.com/serhii-londar/open-source-mac-os-apps | Video | `DATfix - Repair the .DAT file a Canon camera leaves when a recording is cut off.` (match the list's format) |
| https://github.com/jaywcjlove/awesome-mac | Video tools | same, marked free and open source |
| https://github.com/krzemienski/awesome-video | Tools / repair | same |
| https://github.com/rust-unofficial/awesome-rust | Applications > Video | same, with the crate link once published |

### 5.8 Directories

- AlternativeTo (https://alternativeto.net/): add DATfix as an alternative to Wondershare
  Repairit, Grau Video Repair Tool and Stellar Repair for Video.

## 6. After launch

- Record each post (venue, URL, date) in the webmaster journal (`bin/wm journal add action ...`).
- Watch the GitHub issues for camera reports. Move a camera from Expected to Verified only after the
  checks in `AGENTS.md` ("Verifying a repair") pass, then update the README table and the site.
