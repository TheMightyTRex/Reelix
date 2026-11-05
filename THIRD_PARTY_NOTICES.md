# Third-party notices

Reelix is licensed under the GNU GPL v3.0 or later (see `LICENSE`). Parts of it are adapted from the projects below, which are used under their own licenses.

## Storyarr

The BBC radio provider (`crates/reelix-radio/src/providers/bbc.rs`) adapts the endpoints, hierarchy walk, link patterns and programmes A to Z search of Storyarr's BBC adapter, and the stations and categories of its radio Discover: <https://github.com/TheMightyTRex/storyarr>

Reading the BBC programme id from get_iplayer release names (`crates/reelix-parser/src/radio.rs`), matching radio releases by it (`crates/reelix-radio/src/search.rs`) and the wider radio search categories (`crates/reelix-indexers/src/categories.rs`) adapt the same ideas from Storyarr's radio release matching (`services/app/domain/radio_releases.py`).

```text
MIT License

Copyright (c) 2026 Dewarr contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

```
