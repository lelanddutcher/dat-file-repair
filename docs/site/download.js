// Points the download button at the right installer for this visitor's computer.
// Without JavaScript, or if anything below fails, the button keeps its plain link
// to the GitHub releases page. No cookies, no tracking: one request to the public
// GitHub API for the latest release's file list.
(function () {
  "use strict";
  var REPO = "lelanddutcher/dat-file-repair";
  var RELEASES = "https://github.com/" + REPO + "/releases/latest";
  // The list endpoint answers 200 with [] before the first release, where
  // /releases/latest answers 404 and logs a console error.
  var API = "https://api.github.com/repos/" + REPO + "/releases?per_page=10";
  var ASSET = {
    mac: { suffix: "_macos_universal.dmg", label: "Download for Mac" },
    windows: { suffix: "_windows_x64-setup.exe", label: "Download for Windows" }
  };

  function detectOS() {
    var uad = navigator.userAgentData;
    var platform = (uad && uad.platform) || "";
    var ua = navigator.userAgent || "";
    // Phones and tablets cannot run DATfix. iPadOS reports "Macintosh", so check touch too.
    if (/Android|iPhone|iPad|iPod/i.test(ua) || (uad && uad.mobile)) return "mobile";
    if (/Mac/i.test(platform) || /Macintosh|Mac OS X/i.test(ua)) {
      return navigator.maxTouchPoints > 1 ? "mobile" : "mac";
    }
    if (/Win/i.test(platform) || /Windows/i.test(ua)) return "windows";
    return "other";
  }

  function start() {
    var button = document.getElementById("dl");
    var note = document.getElementById("dl-note");
    if (!button) return;
    var os = detectOS();
    button.setAttribute("data-os", os);

    if (os === "mobile") {
      button.textContent = "See downloads for Mac and Windows";
      if (note) note.textContent = "DATfix runs on a Mac or Windows computer. Copy the card there first.";
      return;
    }
    if (!ASSET[os]) return; // Linux and others: the releases page lists the CLI archives.

    button.textContent = ASSET[os].label;
    button.href = RELEASES; // until the exact file is known

    if (!window.fetch) return;
    fetch(API, { headers: { Accept: "application/vnd.github+json" }, credentials: "omit", referrerPolicy: "no-referrer" })
      .then(function (r) { if (!r.ok) throw new Error("HTTP " + r.status); return r.json(); })
      .then(function (list) {
        var release = null;
        for (var j = 0; j < (list || []).length; j++) {
          if (!list[j].draft && !list[j].prerelease) { release = list[j]; break; }
        }
        var assets = (release && release.assets) || [];
        for (var i = 0; i < assets.length; i++) {
          var name = assets[i].name || "";
          if (name.slice(-ASSET[os].suffix.length) === ASSET[os].suffix) {
            button.href = assets[i].browser_download_url;
            button.setAttribute("data-asset", name);
            if (release.tag_name) button.title = name + " (" + release.tag_name + ")";
            return;
          }
        }
      })
      .catch(function () { /* keep the link to the releases page */ });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
