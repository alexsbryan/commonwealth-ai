# fineprint-data spike: ToS;DR points as held-out truth for OTA policy text

Run 2026-09-19. Numbers come from `results.json`, written by `spike.py`; raw responses are cached under `raw/` (15 MB). `ota-pga-versions/` is a nested git clone: delete or ignore it before committing.

## 1. Re-anchor (approved points, 8 services)

SAMPLE, not census: `quote_text` is absent from `service/v3` and costs one `point/v1?id=` request per point, so 484 approved points did not fit the 400-request budget. Per service: every point already returned by the three case listings, plus a seeded (20260919) random draw of up to 26 others. Exact = normalised quote is a substring. 12w = some contiguous 12-word window of the quote occurs (quotes of 12 words or fewer fall back to exact). Normalisation: strip HTML tags, unescape entities, markdown links to anchor text, lowercase, every non-alphanumeric run to one space.

| service | approved | tested | (a) ToS;DR exact | (a) 12w | (b) OTA exact | (b) 12w | OTA docs |
|---|---|---|---|---|---|---|---|
| Reddit | 96 | 29 | 29/29 (100%) | 29/29 (100%) | 28/29 (97%) | 28/29 (97%) | 12 |
| Spotify | 74 | 29 | 26/29 (90%) | 29/29 (100%) | 26/29 (90%) | 29/29 (100%) | 9 |
| LinkedIn | 20 | 20 | 19/20 (95%) | 20/20 (100%) | 18/20 (90%) | 19/20 (95%) | 13 |
| Zoom | 57 | 27 | 27/27 (100%) | 27/27 (100%) | 1/27 (4%) | 9/27 (33%) | 2 |
| Discord | 87 | 28 | 27/28 (96%) | 27/28 (96%) | 24/28 (86%) | 24/28 (86%) | 3 |
| Google | 62 | 28 | 25/28 (89%) | 27/28 (96%) | 16/28 (57%) | 27/28 (96%) | 2 |
| Facebook | 65 | 29 | 29/29 (100%) | 29/29 (100%) | 12/29 (41%) | 19/29 (66%) | 19 |
| Slack | 23 | 23 | 23/23 (100%) | 23/23 (100%) | no OTA folder | - | 0 |
| ALL | 484 | 213 | 205/213 (96%) | 211/213 (99%) | 125/213 (59%) | 155/213 (73%) | |
| ALL, OTA-covered services only | | 190 | | | 125/190 (66%) | 155/190 (82%) | |

OTA documents used: contrib-versions files for 7 services (fetched singly; the repo is 74,981 KB by the GitHub `size` field, over the 60 MB clone limit, so it was NOT cloned) plus the pga-versions shallow clone (7.6 MB on disk; Reddit, Spotify, LinkedIn, Facebook). Slack has no folder in any of the four repos listed.

Why (b) misses, from the per-document split: Zoom's ToS;DR documents were last crawled 2020-12-14, OTA holds today's text (1/27 exact). Facebook's ToS;DR crawl is of `mbasic.facebook.com` printable pages and an AI-terms page OTA does not track (0/4 exact on AI Terms). Google Privacy Policy is 11/21 exact but 21/21 on the 12-word window, i.e. formatting and list-boundary differences, not drift.

## 2. OTA repos

| repo | size (KB, GitHub API) | service folders | of the 8 present |
|---|---|---|---|
| contrib-versions | 74981 | 387 | Reddit, Spotify, LinkedIn, Zoom, Discord, Google, Facebook (7) |
| pga-versions | 7999 | 26 | Reddit, Spotify, LinkedIn, Facebook (4) |
| vlopses-us-versions | 8556 | 23 | LinkedIn, Facebook (2) |
| genai-contrib-versions | 8941 | 51 | none (has 'Google Generative AI Services') |

Document types present per service:

- Reddit: contrib:Live Policy, contrib:Privacy Policy, contrib:Quality Guidelines, contrib:Terms of Service, pga:Advertising Content Policy, pga:Community Guidelines, pga:Developer Terms, pga:Live Policy, pga:Privacy Policy, pga:Quality Guidelines, pga:Terms of Service, pga:Trackers Policy
- Spotify: contrib:Privacy Policy, contrib:Terms of Service, pga:Acceptable Use Policy, pga:Advertising Content Policy, pga:Community Guidelines, pga:Content Monetisation Policy, pga:Privacy Policy, pga:Terms of Service, pga:Trackers Policy
- LinkedIn: contrib:Brand Guidelines, contrib:Community Guidelines, contrib:Developer Terms, contrib:Privacy Policy, contrib:Terms of Service, contrib:Trackers Policy, pga:Advertising Content Policy, pga:Brand Guidelines, pga:Community Guidelines, pga:Developer Terms, pga:Privacy Policy, pga:Terms of Service, pga:Trackers Policy
- Zoom: contrib:Privacy Policy, contrib:Terms of Service
- Discord: contrib:Community Guidelines, contrib:Privacy Policy, contrib:Terms of Service
- Google: contrib:Privacy Policy, contrib:Terms of Service
- Facebook: contrib:Commercial Terms, contrib:Data Processor Agreement, contrib:Developer Terms, contrib:Law Enforcement Guidelines, contrib:Live Policy, contrib:Privacy Policy, contrib:Terms of Service, contrib:Trackers Policy, pga:Advertising Content Policy, pga:Commercial Terms, pga:Community Guidelines, pga:Data Processor Agreement, pga:Developer Terms, pga:Law Enforcement Guidelines, pga:Live Policy, pga:Privacy Policy, pga:Terms of Service, pga:Trackers Policy, pga:User Consent Policy
- Slack: none

## 3. Set sizes

A point in a case listing carries NO service id, and `source` is null on most recent points (48 of 54 approved in case 504). Service identity therefore needs `document/v1?id=` (one request per document, returns `service_id`). That was affordable for case 504 only (36 documents); 220 and 166 are resolved where a document was already cached or `source` host matched a ToS;DR service `urls` entry, and extrapolated for the rest. The resolved subset over-represents the 8 large services fetched in step 1, all of which are in OTA, so the extrapolated in-OTA figures lean HIGH. OTA membership is a case-insensitive alphanumeric name match against folder names in the four repos: approximate.

| case | points (all statuses) | approved points | distinct approved docs (upper bound on services) | services resolved | of which in OTA | extrapolated services | extrapolated in OTA |
|---|---|---|---|---|---|---|---|
| 220 targeted third-party advertising | 1797 {'declined': 135, 'approved': 241, 'pending': 1420, 'changes-requested': 1} | 241 | 220 | 154 | 26 | 220 | 37 |
| 504 automated decisions / profiling / AI training | 73 {'approved': 54, 'declined': 19} | 54 | 54 | 48 | 17 | 54 | 19 |
| 166 shares data with non-essential third parties | 3294 {'approved': 199, 'declined': 211, 'pending': 2882, 'changes-requested': 2} | 199 | 188 | 140 | 18 | 188 | 24 |

Case 220 resolved services found in OTA: BBC, Blizzard, Brave, Bumble, Facebook, Foursquare, Google, IKEA, Imgur, Instagram, Last.fm, Microsoft Store, Netflix, Quora, Rakuten, Reddit, Samsung, SoundCloud, Spotify, The Guardian, TikTok, Tinder, W3Schools, WeTransfer, XING, eBay

Case 504 resolved services found in OTA: BlueSky, Brave, DeviantArt, Discord, Facebook, Foursquare, Google, Grammarly, IKEA, Last.fm, PayPal, Pinterest, Reddit, Samsung, Spotify, Tinder, Wire

Case 166 resolved services found in OTA: BlueSky, Brave, Coinbase, Discord, Disqus, Facebook, Google, Instagram, Netflix, Qwant, Reddit, Samsung, SoundCloud, Spotify, TikTok, Tinder, W3Schools, eBay

## 4. Overlap, OTA folders vs ToS;DR service names

CENSUS, not a sample: the full ToS;DR service list is 21 pages of 500 (10,290 services), so all 451 distinct OTA folder names were tested. Exact match on lowercased alphanumerics only, so 'Meta' vs 'Facebook' style renames and product sub-folders ('Google Ads') count as misses: a lower bound.

| repo | folders | match a ToS;DR name |
|---|---|---|
| contrib-versions | 387 | 174 |
| pga-versions | 26 | 20 |
| vlopses-us-versions | 23 | 14 |
| genai-contrib-versions | 51 | 8 |
| _union | 451 | 190 |

## 5. Case 504, 15 approved quotes (seeded draw), labelled by hand

Tally: {"AI-TRAINING": 6, "UNCLEAR": 4, "PROFILING-ONLY": 5}

- **AI-TRAINING** (point 324844): | To customize your experience with our Services and otherwise improve our Services including, depending on the terms that apply to your use of the Services, using User Content to train, fine tune and improve the models that power our Services. | Basic Information, Account Information, Communications, App, browser, and device information, Services usage data, and User Content | Legitimate Interests — specifically, it
- **UNCLEAR** (point 330499): <p>We use third-party service providers to offer AI-powered chatbot and search tools on our website to assist with general inquiries and navigation. Your interactions with these tools may be recorded, including message content and usage patterns, and the data may be used to help improve response accuracy, personalize your experience, and enhance system performance.
- **UNCLEAR** (point 319091): This processing can include use of automated technologies such as machine learning systems that help us power and improve our services.
- **PROFILING-ONLY** (point 318417): In certain cases we make automated decisions related to you. We may use technology to assess your personal situation and other factors to predict potential risks or outcomes (this practice is known as profiling).
- **PROFILING-ONLY** (point 319376): Wealthsimple may use automated decision making and artificial intelligence systems to process your personal information. Wealthsimple may rely on decisions made by these automated systems for the following purposes:</p> <ul> <li><p>providing our Products or services to you (e.g. verifying your identity, assessing internal risk or creditworthiness);</p></li> <li><p>customer service purposes (e.g. using AI powered chat
- **AI-TRAINING** (point 327901): Train machine learning models to improve services, for example, better recommendations
- **UNCLEAR** (point 319478): <br><br>Developing and improving new features and services, including through machine learning and other technologies, and testing them out<br><br>
- **AI-TRAINING** (point 319274): * We use publicly available information online or from other public sources to help train new machine learning models and build foundational technologies that power various Google products such as Google Translate, Gemini Apps, and Cloud AI capabilities.<br>* We use your interactions with AI models and technologies like Gemini Apps to develop, train, fine-tune, and improve these models to better handle your requests,
- **PROFILING-ONLY** (point 330102): We may use automated decision making and/or profiling regarding your personal data for some Services, for example by providing you with relevant features suggestion or by offering you a custom promotional offer based on your email information and behavior inside our Services. You can request a manual review of the accuracy of an automated decision that you are unhappy with or limit or object to such automated decisio
- **PROFILING-ONLY** (point 322237): <p><strong>Right Not to Be Subject to Automated Individual Decision-Making.</strong> You have the right not to be subject to a decision based solely on automated processing (including profiling) that produces legal effects concerning you or similarly significantly affects you.</p>
- **UNCLEAR** (point 327774): To conduct data analytics and use artificial intelligence and machine learning to support internal research, create new products and services, improve functionality of existing products and services, and develop new features. nd develop new features.
- **AI-TRAINING** (point 319918): to monitor how the Services are used and to develop new products and services, including to train AI models,
- **AI-TRAINING** (point 327989): <p>We use Personal Information that we collect on the Scratch Website, such as your location and your activities, to monitor and analyze usage of the Scratch Website and to enhance your learning experience,&nbsp;including by training AI tools to personalize features or recommendations on the Website.</p>
- **PROFILING-ONLY** (point 321325): In certain circumstances, you can object to the processing of your personal data (for example to object to the use of your data for profiling and/or purely automated decision making).
- **AI-TRAINING** (point 322460): Some features in Asana are powered by artificial intelligence (AI) and machine learning. Admins and super admins can adjust AI preferences for your domain at any time by visiting the admin console.</p> <p>When features powered by Asana AI are enabled in your domain, we use metadata related to your domain’s use of Asana to train machine learning models. Depending on the model and the feature, these machine learning mo

## 6. API facts that did not hold

- `service/v3?id=` points have NO `quote_text` (and no quote offsets); only `point/v1?id=` and `point/v1?case_id=` return it.
- Points carry no service id anywhere. `point/v1` accepts only `id` or `case_id` (`service_id` returns HTTP 400 'Pass `id` or `case_id` and not both').
- `service/v3?id=` returned only approved points for all 8 services (no declined or pending), while case listings return every status. Statuses seen: approved, declined, pending, changes-requested.
- `source` is null on most recent points; do not key on it.
- `document/v1?page=N` lists `[id, null]` pairs only: no way to bulk-map document to service.
- api.tosdr.org returned 429 on the 7th request at about 2 req/s; the run used 1.6 s spacing after that.
- `search/v5?query=` exists and works. Page sizes: services 500, points 100, cases 100.
- `case/v2` and `document/v1` wrap the payload in `parameters` with `error: 256` meaning OK; `point/v1` and `service/v3` do not.
- Some points have `document_id: null` (2 of 484); Google has 2 approved points on document 347, which its own `documents` list omits.
- OTA repo names verified as given. `contrib-versions` exceeds 60 MB by the API size field.

## 7. Requests

Total 400 (limit was UNDER 400, so this is at the cap, not under it). 396 were planned; 4 extra `document/v1` calls came from a re-run bug in the case-504 document sample, which re-derived itself from the cache. It is now pinned in `case504_doc_sample.json` and a re-run costs zero requests. Non-200: [["https://api.tosdr.org/point/v1?service_id=194", 400], ["https://api.tosdr.org/search/v5?query=facebook", 429]]. User-Agent was the literal `curl/8.7.1` throughout; no identifying data was sent. Plus one `git clone --depth 1` of pga-versions.

## 8. Commands

```
git clone --depth 1 https://github.com/OpenTermsArchive/pga-versions.git ota-pga-versions
python3 spike.py > run.log     # fetch.py = cached, rate-limited, counted GET; raw/_requests.json is the request log
```

Files: `spike.py`, `fetch.py`, `case504_labels.json` (hand labels), `case504_doc_sample.json` (pinned sample), `results.json`, `run.log`, `raw/`.
