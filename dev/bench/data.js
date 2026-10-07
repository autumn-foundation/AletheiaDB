{
  "lastUpdate": 1791391270035,
  "repoUrl": "https://github.com/autumn-foundation/AletheiaDB",
  "entries": {
    "AletheiaDB Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "49699333+dependabot[bot]@users.noreply.github.com",
            "name": "dependabot[bot]",
            "username": "dependabot[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "9b723d6da3a1c1a410e871ca4f39d58bc8183f0a",
          "message": "chore(deps)(deps): bump tower-http from 0.6.10 to 0.6.11 (#2924)\n\nBumps [tower-http](https://github.com/tower-rs/tower-http) from 0.6.10\nto 0.6.11.\n<details>\n<summary>Release notes</summary>\n<p><em>Sourced from <a\nhref=\"https://github.com/tower-rs/tower-http/releases\">tower-http's\nreleases</a>.</em></p>\n<blockquote>\n<h2>tower-http-0.6.11</h2>\n<h2>Added</h2>\n<ul>\n<li>\n<p><code>set-header</code>: add\n<code>SetMultipleResponseHeadersLayer</code> and\n<code>SetMultipleResponseHeader</code> for setting multiple response\nheaders at once.\nSupports <code>overriding</code>, <code>appending</code>, and\n<code>if_not_present</code> modes. Header\nvalues can be fixed or computed dynamically via closures (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/672\">#672</a>)</p>\n<pre lang=\"rust\"><code>use http::{Response, header::{self,\nHeaderValue}};\nuse http_body::Body as _;\nuse tower_http::set_header::response::SetMultipleResponseHeadersLayer;\n<p>let layer = SetMultipleResponseHeadersLayer::overriding(vec![<br />\n(header::X_FRAME_OPTIONS,\nHeaderValue::from_static(&quot;DENY&quot;)).into(),<br />\n(header::CONTENT_LENGTH, |res: &amp;Response&lt;MyBody&gt;| {<br />\nres.body().size_hint().exact()<br />\n.map(|size| HeaderValue::from_str(&amp;size.to_string()).unwrap())<br />\n}).into(),<br />\n]);<br />\n</code></pre></p>\n</li>\n<li>\n<p><code>set-header</code>: add\n<code>SetMultipleRequestHeadersLayer</code> and\n<code>SetMultipleRequestHeaders</code> for setting multiple request\nheaders at once,\nmirroring the response-side API (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/677\">#677</a>)</p>\n</li>\n<li>\n<p><code>classify</code>: add <code>From&lt;i32&gt;</code> and\n<code>From&lt;NonZeroI32&gt;</code> impls for <code>GrpcCode</code>.\nUnrecognized status codes map to <code>GrpcCode::Unknown</code> (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/506\">#506</a>)</p>\n</li>\n</ul>\n<h2>Changed</h2>\n<ul>\n<li><code>compression</code>: compress <code>application/grpc-web</code>\nresponses. Previously all\n<code>application/grpc*</code> content types were excluded from\ncompression; now only\n<code>application/grpc</code> (non-web) is excluded (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/408\">#408</a>)</li>\n</ul>\n<h2>Fixed</h2>\n<ul>\n<li><code>fs</code>: fix <code>ServeDir</code> returning 500 instead of\n405 for non-GET/HEAD requests\nwhen <code>call_fallback_on_method_not_allowed</code> is enabled but no\nfallback service\nis configured (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/587\">#587</a>)</li>\n<li><code>fs</code>: remove duplicate <code>cfg</code> attribute on\n<code>is_reserved_dos_name</code> (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/675\">#675</a>)</li>\n</ul>\n<p><a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/408\">#408</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/408\">tower-rs/tower-http#408</a>\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/506\">#506</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/506\">tower-rs/tower-http#506</a>\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/587\">#587</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/587\">tower-rs/tower-http#587</a>\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/672\">#672</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/672\">tower-rs/tower-http#672</a>\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/675\">#675</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/675\">tower-rs/tower-http#675</a>\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/677\">#677</a>:\n<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/677\">tower-rs/tower-http#677</a></p>\n<h2>All PRs</h2>\n<ul>\n<li>ci: fix flaky encoding test, add nightly stress test job by <a\nhref=\"https://github.com/jlizen\"><code>@​jlizen</code></a> in <a\nhref=\"https://redirect.github.com/tower-rs/tower-http/pull/670\">tower-rs/tower-http#670</a></li>\n</ul>\n<!-- raw HTML omitted -->\n</blockquote>\n<p>... (truncated)</p>\n</details>\n<details>\n<summary>Commits</summary>\n<ul>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/1d082ef7bdb6d80a2964698804a46c338b4c6a99\"><code>1d082ef</code></a>\nv0.6.11</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/9c3117d856986336ca0662ca7c78318e724e0fda\"><code>9c3117d</code></a>\nfeat: set multiple request header (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/677\">#677</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/667e7c7a7c109488479b1e9c1d57093dbeb6d867\"><code>667e7c7</code></a>\nRemove duplicate cfg attribute for is_reserved_dos_name (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/675\">#675</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/7551a9b8b9706ca1e11c035659b243f688b136bd\"><code>7551a9b</code></a>\nfeat(set_header): refactor and improve multiple header middleware (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/672\">#672</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/991e9ee595882626fe3a0b3ceec3df54d4e7f9b5\"><code>991e9ee</code></a>\nadd From&lt;i32&gt; impl for GrpcCode (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/506\">#506</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/3962dbab7b74b8543a8baafa3dae49af06fb8fd7\"><code>3962dba</code></a>\nDo compress grpc-web responses (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/408\">#408</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/f0b3bb6dcde9996d11d0b820c7dd1006bbdf9f23\"><code>f0b3bb6</code></a>\nFix serve_dir method not allowed handling when no fallback is configured\n(<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/587\">#587</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/d1a571bdeb2cb0e92f0670b09a4309b8e97cab9f\"><code>d1a571b</code></a>\nci: use static timeout in stress-test workflow (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/671\">#671</a>)</li>\n<li><a\nhref=\"https://github.com/tower-rs/tower-http/commit/309555a6a2f0b5343e1bd5aaea958d9e873150b3\"><code>309555a</code></a>\nci: fix flaky encoding test, add nightly stress test job (<a\nhref=\"https://redirect.github.com/tower-rs/tower-http/issues/670\">#670</a>)</li>\n<li>See full diff in <a\nhref=\"https://github.com/tower-rs/tower-http/compare/tower-http-0.6.10...tower-http-0.6.11\">compare\nview</a></li>\n</ul>\n</details>\n<br />\n\n\n[![Dependabot compatibility\nscore](https://dependabot-badges.githubapp.com/badges/compatibility_score?dependency-name=tower-http&package-manager=cargo&previous-version=0.6.10&new-version=0.6.11)](https://docs.github.com/en/github/managing-security-vulnerabilities/about-dependabot-security-updates#about-compatibility-scores)\n\nYou can trigger a rebase of this PR by commenting `@dependabot rebase`.\n\n[//]: # (dependabot-automerge-start)\n[//]: # (dependabot-automerge-end)\n\n---\n\n<details>\n<summary>Dependabot commands and options</summary>\n<br />\n\nYou can trigger Dependabot actions by commenting on this PR:\n- `@dependabot rebase` will rebase this PR\n- `@dependabot recreate` will recreate this PR, overwriting any edits\nthat have been made to it\n- `@dependabot show <dependency name> ignore conditions` will show all\nof the ignore conditions of the specified dependency\n- `@dependabot ignore this major version` will close this PR and stop\nDependabot creating any more for this major version (unless you reopen\nthe PR or upgrade to it yourself)\n- `@dependabot ignore this minor version` will close this PR and stop\nDependabot creating any more for this minor version (unless you reopen\nthe PR or upgrade to it yourself)\n- `@dependabot ignore this dependency` will close this PR and stop\nDependabot creating any more for this dependency (unless you reopen the\nPR or upgrade to it yourself)\n\n\n</details>\n\n> **Note**\n> Automatic rebases have been disabled on this pull request as it has\nbeen open for over 30 days.\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-10-07T11:01:21-05:00",
          "tree_id": "5d20f4c69b79435d074ab62a446ee6852c46e668",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/9b723d6da3a1c1a410e871ca4f39d58bc8183f0a"
        },
        "date": 1791391270034,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 20.61827343078154,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 173.02834259427607,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 209.4795710877638,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 209.7727642610503,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 163.72439521671177,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 390716.5338762293,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}