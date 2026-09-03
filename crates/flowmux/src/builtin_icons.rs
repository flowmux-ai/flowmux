// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in fallback icons for GTK symbolic names used by flowmux.

use std::path::{Path, PathBuf};

struct SymbolicIcon {
    name: &'static str,
    path: &'static str,
}

struct AgentIcon {
    agent: &'static str,
    name: &'static str,
    svg: &'static str,
}

const INDEX_THEME: &str = r#"[Icon Theme]
Name=FlowMux Builtin
Comment=Built-in fallback icons for flowmux
Directories=scalable/actions,scalable/apps

[scalable/actions]
Context=Actions
Type=Scalable
Size=16
MinSize=8
MaxSize=64

[scalable/apps]
Context=Applications
Type=Scalable
Size=16
MinSize=8
MaxSize=512
"#;

const APP_ICON_SVG: &str = include_str!("../../../resources/icons/flowmux.svg");
const FALLBACK_AGENT_ICON: &str = "applications-utilities-symbolic";

// Lobe Icons (MIT), pinned at 4aaf4ee1fb2678a7f989ea570f0f6ce14a9abf75.
// Aider (Apache-2.0), pinned at 5dc9490bb35f9729ef2c95d00a19ccd30c26339c.
const AGENT_ICONS: &[AgentIcon] = &[
    AgentIcon {
        agent: "codex",
        name: "flowmux-agent-codex",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Codex</title><path d="M19.503 0H4.496A4.496 4.496 0 000 4.496v15.007A4.496 4.496 0 004.496 24h15.007A4.496 4.496 0 0024 19.503V4.496A4.496 4.496 0 0019.503 0z" fill="#fff"></path><path d="M9.064 3.344a4.578 4.578 0 012.285-.312c1 .115 1.891.54 2.673 1.275.01.01.024.017.037.021a.09.09 0 00.043 0 4.55 4.55 0 013.046.275l.047.022.116.057a4.581 4.581 0 012.188 2.399c.209.51.313 1.041.315 1.595a4.24 4.24 0 01-.134 1.223.123.123 0 00.03.115c.594.607.988 1.33 1.183 2.17.289 1.425-.007 2.71-.887 3.854l-.136.166a4.548 4.548 0 01-2.201 1.388.123.123 0 00-.081.076c-.191.551-.383 1.023-.74 1.494-.9 1.187-2.222 1.846-3.711 1.838-1.187-.006-2.239-.44-3.157-1.302a.107.107 0 00-.105-.024c-.388.125-.78.143-1.204.138a4.441 4.441 0 01-1.945-.466 4.544 4.544 0 01-1.61-1.335c-.152-.202-.303-.392-.414-.617a5.81 5.81 0 01-.37-.961 4.582 4.582 0 01-.014-2.298.124.124 0 00.006-.056.085.085 0 00-.027-.048 4.467 4.467 0 01-1.034-1.651 3.896 3.896 0 01-.251-1.192 5.189 5.189 0 01.141-1.6c.337-1.112.982-1.985 1.933-2.618.212-.141.413-.251.601-.33.215-.089.43-.164.646-.227a.098.098 0 00.065-.066 4.51 4.51 0 01.829-1.615 4.535 4.535 0 011.837-1.388zm3.482 10.565a.637.637 0 000 1.272h3.636a.637.637 0 100-1.272h-3.636zM8.462 9.23a.637.637 0 00-1.106.631l1.272 2.224-1.266 2.136a.636.636 0 101.095.649l1.454-2.455a.636.636 0 00.005-.64L8.462 9.23z" fill="url(#lobe-icons-codex-_R_0_)"></path><defs><linearGradient gradientUnits="userSpaceOnUse" id="lobe-icons-codex-_R_0_" x1="12" x2="12" y1="3" y2="21"><stop stop-color="#B1A7FF"></stop><stop offset=".5" stop-color="#7A9DFF"></stop><stop offset="1" stop-color="#3941FF"></stop></linearGradient></defs></svg>"##,
    },
    AgentIcon {
        agent: "claude",
        name: "flowmux-agent-claude",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Claude Code</title><path clip-rule="evenodd" d="M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z" fill="#D97757" fill-rule="evenodd"></path></svg>"##,
    },
    AgentIcon {
        agent: "opencode",
        name: "flowmux-agent-opencode",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>opencode</title><rect width="24" height="24" rx="2" fill="#000"></rect><path d="M16 6H8v12h8V6zm4 16H4V2h16v20z" fill="#fff" transform="translate(3 3) scale(.75)"></path></svg>"##,
    },
    AgentIcon {
        agent: "cline",
        name: "flowmux-agent-cline",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Cline</title><rect width="24" height="24" rx="2" fill="#323B43"></rect><g fill="#fff" transform="translate(4.8 4.8) scale(.6)"><path d="M17.035 3.991c2.75 0 4.98 2.24 4.98 5.003v1.667l1.45 2.896a1.01 1.01 0 01-.002.909l-1.448 2.864v1.668c0 2.762-2.23 5.002-4.98 5.002H7.074c-2.751 0-4.98-2.24-4.98-5.002V17.33l-1.48-2.855a1.01 1.01 0 01-.003-.927l1.482-2.887V8.994c0-2.763 2.23-5.003 4.98-5.003h9.962zM8.265 9.6a2.274 2.274 0 00-2.274 2.274v4.042a2.274 2.274 0 004.547 0v-4.042A2.274 2.274 0 008.265 9.6zm7.326 0a2.274 2.274 0 00-2.274 2.274v4.042a2.274 2.274 0 104.548 0v-4.042A2.274 2.274 0 0015.59 9.6z"></path><path d="M12.054 5.558a2.779 2.779 0 100-5.558 2.779 2.779 0 000 5.558z"></path></g></svg>"##,
    },
    AgentIcon {
        agent: "gemini",
        name: "flowmux-agent-gemini",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Gemini CLI</title><path d="M0 4.391A4.391 4.391 0 014.391 0h15.217A4.391 4.391 0 0124 4.391v15.217A4.391 4.391 0 0119.608 24H4.391A4.391 4.391 0 010 19.608V4.391z" fill="url(#lobe-icons-gemini-cli-_R_0_)"></path><path clip-rule="evenodd" d="M19.74 1.444a2.816 2.816 0 012.816 2.816v15.48a2.816 2.816 0 01-2.816 2.816H4.26a2.816 2.816 0 01-2.816-2.816V4.26A2.816 2.816 0 014.26 1.444h15.48zM7.236 8.564l7.752 3.728-7.752 3.727v2.802l9.557-4.596v-3.866L7.236 5.763v2.801z" fill="#1E1E2E" fill-rule="evenodd"></path><defs><linearGradient gradientUnits="userSpaceOnUse" id="lobe-icons-gemini-cli-_R_0_" x1="24" x2="0" y1="6.587" y2="16.494"><stop stop-color="#EE4D5D"></stop><stop offset=".328" stop-color="#B381DD"></stop><stop offset=".476" stop-color="#207CFE"></stop></linearGradient></defs></svg>"##,
    },
    AgentIcon {
        agent: "antigravity",
        name: "flowmux-agent-antigravity",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Antigravity</title><mask height="23" id="lobe-icons-antigravity-0-_R_0_" maskUnits="userSpaceOnUse" width="24" x="0" y="1"><path d="M21.751 22.607c1.34 1.005 3.35.335 1.508-1.508C17.73 15.74 18.904 1 12.037 1 5.17 1 6.342 15.74.815 21.1c-2.01 2.009.167 2.511 1.507 1.506 5.192-3.517 4.857-9.714 9.715-9.714 4.857 0 4.522 6.197 9.714 9.715z" fill="#fff"></path></mask><g mask="url(#lobe-icons-antigravity-0-_R_0_)"><g filter="url(#lobe-icons-antigravity-1-_R_0_)"><path d="M-1.018-3.992c-.408 3.591 2.686 6.89 6.91 7.37 4.225.48 7.98-2.043 8.387-5.633.408-3.59-2.686-6.89-6.91-7.37-4.225-.479-7.98 2.043-8.387 5.633z" fill="#FFE432"></path></g><g filter="url(#lobe-icons-antigravity-2-_R_0_)"><path d="M15.269 7.747c1.058 4.557 5.691 7.374 10.348 6.293 4.657-1.082 7.575-5.653 6.516-10.21-1.058-4.556-5.691-7.374-10.348-6.292-4.657 1.082-7.575 5.653-6.516 10.21z" fill="#FC413D"></path></g><g filter="url(#lobe-icons-antigravity-3-_R_0_)"><path d="M-12.443 10.804c1.338 4.703 7.36 7.11 13.453 5.378 6.092-1.733 9.947-6.95 8.61-11.652C8.282-.173 2.26-2.58-3.833-.848-9.925.884-13.78 6.1-12.443 10.804z" fill="#00B95C"></path></g><g filter="url(#lobe-icons-antigravity-4-_R_0_)"><path d="M-12.443 10.804c1.338 4.703 7.36 7.11 13.453 5.378 6.092-1.733 9.947-6.95 8.61-11.652C8.282-.173 2.26-2.58-3.833-.848-9.925.884-13.78 6.1-12.443 10.804z" fill="#00B95C"></path></g><g filter="url(#lobe-icons-antigravity-5-_R_0_)"><path d="M-7.608 14.703c3.352 3.424 9.126 3.208 12.896-.483 3.77-3.69 4.108-9.459.756-12.883C2.69-2.087-3.083-1.871-6.853 1.82c-3.77 3.69-4.108 9.458-.755 12.883z" fill="#00B95C"></path></g><g filter="url(#lobe-icons-antigravity-6-_R_0_)"><path d="M9.932 27.617c1.04 4.482 5.384 7.303 9.7 6.3 4.316-1.002 6.971-5.448 5.93-9.93-1.04-4.483-5.384-7.304-9.7-6.301-4.316 1.002-6.971 5.448-5.93 9.93z" fill="#3186FF"></path></g><g filter="url(#lobe-icons-antigravity-7-_R_0_)"><path d="M2.572-8.185C.392-3.329 2.778 2.472 7.9 4.771c5.122 2.3 11.042.227 13.222-4.63 2.18-4.855-.205-10.656-5.327-12.955-5.122-2.3-11.042-.227-13.222 4.63z" fill="#FBBC04"></path></g><g filter="url(#lobe-icons-antigravity-8-_R_0_)"><path d="M-3.267 38.686c-5.277-2.072 3.742-19.117 5.984-24.83 2.243-5.712 8.34-8.664 13.616-6.592 5.278 2.071 11.533 13.482 9.29 19.195-2.242 5.713-23.613 14.298-28.89 12.227z" fill="#3186FF"></path></g><g filter="url(#lobe-icons-antigravity-9-_R_0_)"><path d="M28.71 17.471c-1.413 1.649-5.1.808-8.236-1.878-3.135-2.687-4.531-6.201-3.118-7.85 1.412-1.649 5.1-.808 8.235 1.878s4.532 6.2 3.119 7.85z" fill="#749BFF"></path></g><g filter="url(#lobe-icons-antigravity-10-_R_0_)"><path d="M18.163 9.077c5.81 3.93 12.502 4.19 14.946.577 2.443-3.612-.287-9.727-6.098-13.658-5.81-3.931-12.502-4.19-14.946-.577-2.443 3.612.287 9.727 6.098 13.658z" fill="#FC413D"></path></g><g filter="url(#lobe-icons-antigravity-11-_R_0_)"><path d="M-.915 2.684c-1.44 3.473-.97 6.967 1.05 7.804 2.02.837 4.824-1.3 6.264-4.772 1.44-3.473.97-6.967-1.05-7.804-2.02-.837-4.824 1.3-6.264 4.772z" fill="#FFEE48"></path></g></g><defs><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="17.587" id="lobe-icons-antigravity-1-_R_0_" width="19.838" x="-3.288" y="-11.917"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="1.117"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="38.565" id="lobe-icons-antigravity-2-_R_0_" width="38.9" x="4.251" y="-13.493"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="5.4"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="36.517" id="lobe-icons-antigravity-3-_R_0_" width="40.955" x="-21.889" y="-10.592"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="4.591"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="36.517" id="lobe-icons-antigravity-4-_R_0_" width="40.955" x="-21.889" y="-10.592"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="4.591"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="36.595" id="lobe-icons-antigravity-5-_R_0_" width="36.632" x="-19.099" y="-10.278"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="4.591"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="34.087" id="lobe-icons-antigravity-6-_R_0_" width="33.533" x=".981" y="8.758"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="4.363"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="35.276" id="lobe-icons-antigravity-7-_R_0_" width="35.978" x="-6.143" y="-21.659"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="3.954"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="46.523" id="lobe-icons-antigravity-8-_R_0_" width="45.114" x="-11.96" y="-.46"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="3.531"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="24.054" id="lobe-icons-antigravity-9-_R_0_" width="25.094" x="10.485" y=".58"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="3.159"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="30.007" id="lobe-icons-antigravity-10-_R_0_" width="33.508" x="5.833" y="-12.467"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="2.669"></feGaussianBlur></filter><filter color-interpolation-filters="sRGB" filterUnits="userSpaceOnUse" height="26.151" id="lobe-icons-antigravity-11-_R_0_" width="22.194" x="-8.355" y="-8.876"><feFlood flood-opacity="0" result="BackgroundImageFix"></feFlood><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"></feBlend><feGaussianBlur result="effect1_foregroundBlur_977_115" stdDeviation="3.303"></feGaussianBlur></filter></defs></svg>"##,
    },
    AgentIcon {
        agent: "aider",
        name: "flowmux-agent-aider",
        svg: r##"<?xml version="1.0" standalone="no"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 20010904//EN"
 "http://www.w3.org/TR/2001/REC-SVG-20010904/DTD/svg10.dtd">
<svg version="1.0" xmlns="http://www.w3.org/2000/svg"
 width="436.000000pt" height="436.000000pt" viewBox="0 0 436.000000 436.000000"
 preserveAspectRatio="xMidYMid meet">
<metadata>
Created by potrace 1.14, written by Peter Selinger 2001-2017
</metadata>
<g transform="translate(0.000000,436.000000) scale(0.100000,-0.100000)"
fill="#14b014" stroke="none">
<path d="M0 2180 l0 -2180 2180 0 2180 0 0 2180 0 2180 -2180 0 -2180 0 0
-2180z m2705 1818 c20 -20 28 -121 30 -398 l2 -305 216 -5 c118 -3 218 -8 222
-12 3 -3 10 -46 15 -95 5 -48 16 -126 25 -172 17 -86 17 -81 -17 -233 -14 -67
-13 -365 2 -438 21 -100 22 -159 5 -247 -24 -122 -24 -363 1 -458 23 -88 23
-213 1 -330 -9 -49 -17 -109 -17 -132 l0 -43 203 0 c111 0 208 -4 216 -9 10
-6 18 -51 27 -148 8 -76 16 -152 20 -168 7 -39 -23 -361 -37 -387 -10 -18 -21
-19 -214 -16 -135 2 -208 7 -215 14 -22 22 -33 301 -21 501 6 102 8 189 5 194
-8 13 -417 12 -431 -2 -12 -12 -8 -146 8 -261 8 -55 8 -95 1 -140 -6 -35 -14
-99 -17 -143 -9 -123 -14 -141 -41 -154 -18 -8 -217 -11 -679 -11 l-653 0 -11
33 c-31 97 -43 336 -27 533 5 56 6 113 2 128 l-6 26 -194 0 c-211 0 -252 4
-261 28 -12 33 -17 392 -6 522 15 186 -2 174 260 180 115 3 213 8 217 12 4 4
1 52 -5 105 -7 54 -17 130 -22 168 -7 56 -5 91 11 171 10 55 22 130 26 166 4
36 10 72 15 79 7 12 128 15 665 19 l658 5 8 30 c5 18 4 72 -3 130 -12 115 -7
346 11 454 10 61 10 75 -1 82 -8 5 -300 9 -650 9 l-636 0 -27 25 c-18 16 -26
34 -26 57 0 18 -5 87 -10 153 -10 128 5 449 22 472 5 7 26 13 46 15 78 6 1281
3 1287 -4z"/>
<path d="M1360 1833 c0 -5 -1 -164 -3 -356 l-2 -347 625 -1 c704 -1 708 -1
722 7 5 4 7 20 4 38 -29 141 -32 491 -6 595 9 38 8 45 -7 57 -15 11 -139 13
-675 14 -362 0 -658 -3 -658 -7z"/>
</g>
</svg>
"##,
    },
    AgentIcon {
        agent: "goose",
        name: "flowmux-agent-goose",
        svg: r##"<svg height="1em" style="flex:none;line-height:1" viewBox="0 0 24 24" width="1em" xmlns="http://www.w3.org/2000/svg"><title>Goose</title><rect width="24" height="24" rx="2" fill="#fff"></rect><path d="M21.595 23.61c1.167-.254 2.405-.944 2.405-.944l-2.167-1.784a12.124 12.124 0 01-2.695-3.131 12.127 12.127 0 00-3.97-4.049l-.794-.462a1.115 1.115 0 01-.488-.815.844.844 0 01.154-.575c.413-.582 2.548-3.115 2.94-3.44.503-.416 1.065-.762 1.586-1.159.074-.056.148-.112.221-.17.003-.002.007-.004.009-.007.167-.131.325-.272.45-.438.453-.524.563-.988.59-1.193-.061-.197-.244-.639-.753-1.148.319.02.705.272 1.056.569.235-.376.481-.773.727-1.171.165-.266-.08-.465-.086-.471h-.001V3.22c-.007-.007-.206-.25-.471-.086-.567.35-1.134.702-1.639 1.021 0 0-.597-.012-1.305.599a2.464 2.464 0 00-.438.45l-.007.009c-.058.072-.114.147-.17.221-.397.521-.743 1.083-1.16 1.587-.323.391-2.857 2.526-3.44 2.94a.842.842 0 01-.574.153 1.115 1.115 0 01-.815-.488l-.462-.794a12.123 12.123 0 00-4.049-3.97 12.133 12.133 0 01-3.13-2.695L1.332 0S.643 1.238.39 2.405c.352.428 1.27 1.49 2.34 2.302C1.58 4.167.73 3.75.06 3.4c-.103.765-.063 1.92.043 2.816.726.317 1.961.806 3.219 1.066-1.006.236-2.11.278-2.961.262.15.554.358 1.119.64 1.688.119.263.25.52.39.77.452.125 2.222.383 3.164.171l-2.51.897a27.776 27.776 0 002.544 2.726c2.031-1.092 2.494-1.241 4.018-2.238-2.467 2.008-3.108 2.828-3.8 3.67l-.483.678c-.25.351-.469.725-.65 1.117-.61 1.31-1.47 4.1-1.47 4.1-.154.486.202.842.674.674 0 0 2.79-.861 4.1-1.47.392-.182.766-.4 1.118-.65l.677-.483c.227-.187.453-.37.701-.586 0 0 1.705 2.02 3.458 3.349l.896-2.511c-.211.942.046 2.712.17 3.163.252.142.509.272.772.392.569.28 1.134.49 1.688.64-.016-.853.026-1.956.261-2.962.26 1.258.75 2.493 1.067 3.219.895.106 2.051.146 2.816.043a73.87 73.87 0 01-1.308-2.67c.811 1.07 1.874 1.988 2.302 2.34h-.001z" fill="#000" transform="translate(4.2 4.2) scale(.65)"></path></svg>"##,
    },
];

const SYMBOLIC_ICONS: &[SymbolicIcon] = &[
    SymbolicIcon {
        name: "applications-utilities-symbolic",
        path: "M10.8 1.5a3.4 3.4 0 0 0-3.2 4.6L2.4 11.3a1.8 1.8 0 0 0 2.5 2.5l5.2-5.2a3.4 3.4 0 0 0 4.4-4.3l-2.3 2.3-1.8-1.8 2.3-2.3a3.4 3.4 0 0 0-1.9-1z",
    },
    SymbolicIcon {
        name: "emblem-system-symbolic",
        path: GEAR_PATH,
    },
    SymbolicIcon {
        name: "folder-symbolic",
        path: "M1.5 4A1.5 1.5 0 0 1 3 2.5h3L7.5 4H13a1.5 1.5 0 0 1 1.5 1.5v6A1.5 1.5 0 0 1 13 13H3a1.5 1.5 0 0 1-1.5-1.5zM3 5.5v6h10v-6z",
    },
    SymbolicIcon {
        name: "flowmux-split-down-symbolic",
        path: "M3 3h10a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm.5 1.5v7h9v-7zM3.5 7.25h9v1.5h-9z",
    },
    SymbolicIcon {
        name: "flowmux-split-right-symbolic",
        path: "M3 3h10a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm.5 1.5v7h9v-7zM7.25 4.5h1.5v7h-1.5z",
    },
    SymbolicIcon {
        name: "go-down-symbolic",
        path: "M8.8 2.5v8.1l3-3 1.2 1.2-5 5-5-5 1.2-1.2 3 3V2.5z",
    },
    SymbolicIcon {
        name: "go-next-symbolic",
        path: "M8.8 3 13.8 8l-5 5-1.2-1.2 3-3H2.5V7.2h8.1l-3-3z",
    },
    SymbolicIcon {
        name: "go-previous-symbolic",
        path: "M7.2 3 2.2 8l5 5 1.2-1.2-3-3h8.1V7.2H5.4l3-3z",
    },
    SymbolicIcon {
        name: "input-keyboard-symbolic",
        path: "M1.5 4h13v8h-13zM3 5.5v5h10v-5zM4 6.5h1v1H4zm2 0h1v1H6zm2 0h1v1H8zm2 0h1v1h-1zM4 8.5h1v1H4zm2 0h4v1H6zm5 0h1v1h-1z",
    },
    SymbolicIcon {
        name: "list-add-symbolic",
        path: ADD_PATH,
    },
    SymbolicIcon {
        name: "notifications-symbolic",
        path: "M8 14a2 2 0 0 0 1.8-1.2H6.2A2 2 0 0 0 8 14zM4 11.5h8L11 10V7a3 3 0 0 0-2.2-2.9V3a.8.8 0 0 0-1.6 0v1.1A3 3 0 0 0 5 7v3z",
    },
    SymbolicIcon {
        name: "pan-down-symbolic",
        path: "M3 5.2 4.2 4 8 7.8 11.8 4 13 5.2l-5 5z",
    },
    SymbolicIcon {
        name: "pan-end-symbolic",
        path: "M5.2 3 10.2 8l-5 5L4 11.8 7.8 8 4 4.2z",
    },
    SymbolicIcon {
        name: "preferences-system-symbolic",
        path: GEAR_PATH,
    },
    SymbolicIcon {
        name: "tab-new-symbolic",
        path: "M2 3h8l2 2h2v8H2zM3.5 4.5v7h9v-5H9.4l-1.8-2zM7.2 6.2h1.6V8h1.8v1.6H8.8v1.8H7.2V9.6H5.4V8h1.8z",
    },
    SymbolicIcon {
        name: "text-x-generic-symbolic",
        path: "M4 1.5h5l3 3V14H4zM9 2.8V5h2.2zM5.5 7h5v1h-5zm0 2h5v1h-5zm0 2h3v1h-3z",
    },
    SymbolicIcon {
        name: "user-trash-symbolic",
        path: "M5.5 2 6 1h4l.5 1H13v1.5H3V2zm-1 3h7l-.5 8H5zM6 6v5h1V6zm3 0v5h1V6z",
    },
    SymbolicIcon {
        name: "utilities-system-monitor-symbolic",
        path: "M2.3 8.5h2.2v5H2.3zM6.9 5.5h2.2v8H6.9zM11.5 2.5h2.2v11h-2.2z",
    },
    SymbolicIcon {
        name: "utilities-terminal-symbolic",
        path: "M2 3h12v10H2zM3.5 4.5v7h9v-7zM4.5 6l1.8 2-1.8 2h1.8L8 8 6.3 6zM8 9.5h3v1H8z",
    },
    SymbolicIcon {
        name: "vcs-branch-symbolic",
        path: "M4 2a2 2 0 1 0 1.5 3.3v4.4A2 2 0 1 0 7 11.6V9.8c0-.8.6-1.4 1.4-1.4h1.1A2 2 0 1 0 9.5 7H8.4A2.9 2.9 0 0 0 7 7.4V5.3A2 2 0 0 0 4 2zm0 1.3a.7.7 0 1 1 0 1.4.7.7 0 0 1 0-1.4zm7 0a.7.7 0 1 1 0 1.4.7.7 0 0 1 0-1.4zm-5 8a.7.7 0 1 1 0 1.4.7.7 0 0 1 0-1.4z",
    },
    SymbolicIcon {
        name: "view-refresh-symbolic",
        path: "M12.7 5.2A5.2 5.2 0 1 0 13 8h-1.5a3.7 3.7 0 1 1-.9-2.4L8.5 5.6V7h5V2h-1.4z",
    },
    SymbolicIcon {
        name: "web-browser-symbolic",
        path: "M8 1.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM8 3c.7.8 1.1 1.7 1.3 2.5H6.7C6.9 4.7 7.3 3.8 8 3zM3.8 7h2.6a7 7 0 0 0 0 2H3.8a5 5 0 0 1 0-2zm.8 3.5h2.1c.2.9.6 1.8 1.3 2.5a5 5 0 0 1-3.4-2.5zM8 13c-.7-.8-1.1-1.7-1.3-2.5h2.6C9.1 11.3 8.7 12.2 8 13zm1.6-4H6.4a7 7 0 0 1 0-2h3.2a7 7 0 0 1 0 2zM9.3 5.5A8 8 0 0 0 8 3a5 5 0 0 1 3.4 2.5zm0 5h2.1A5 5 0 0 1 8 13c.7-.7 1.1-1.6 1.3-2.5zM9.6 9a7 7 0 0 0 0-2h2.6a5 5 0 0 1 0 2z",
    },
    SymbolicIcon {
        name: "window-close-symbolic",
        path: X_PATH,
    },
    SymbolicIcon {
        name: "dialog-question-symbolic",
        path: "M8 2a3 3 0 0 0-3 3h1.8a1.2 1.2 0 1 1 1.6 1.1c-.7.3-1.3 1-1.3 1.9v1.2h1.8v-1c0-.3.2-.5.5-.7A2.9 2.9 0 0 0 8 2zM7.1 10.6h1.8v1.9H7.1z",
    },
    SymbolicIcon {
        name: "dialog-warning-symbolic",
        path: "M8 2 14.5 13.5H1.5zM7.2 5.5v4h1.6v-4zM7.2 10.8v1.9h1.6v-1.9z",
    },
    SymbolicIcon {
        name: "edit-delete-symbolic",
        path: X_PATH,
    },
    SymbolicIcon {
        name: "edit-find-symbolic",
        path: "M6.5 2a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9zM6.5 3.5a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM9.9 8.9 13.9 12.9 12.8 14 8.8 10z",
    },
    SymbolicIcon {
        name: "emblem-ok-symbolic",
        path: "M2 7.5 3.4 6.1 6.5 9.2 12.6 3.1 14 4.5 6.5 12z",
    },
    SymbolicIcon {
        name: "folder-download-symbolic",
        path: "M6.5 1.5h3v3h2l-3.5 3.5L4.5 4.5h2zM1.5 8h5l1 1h7v5h-13z",
    },
    SymbolicIcon {
        name: "folder-open-symbolic",
        path: "M1.5 3.5A1 1 0 0 1 2.5 3H6l1.5 1.5H13a1 1 0 0 1 1 1v1.5H4.5L2.5 12H2a.5.5 0 0 1-.5-.5zM3.5 12 5.5 7.5H15L13 12z",
    },
    SymbolicIcon {
        name: "media-playback-pause-symbolic",
        path: "M4 3h3v10H4zM9 3h3v10H9z",
    },
    SymbolicIcon {
        name: "process-stop-symbolic",
        path: "M4 3.5h8a.5.5 0 0 1 .5.5v8a.5.5 0 0 1-.5.5H4a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5z",
    },
    SymbolicIcon {
        name: "process-working-symbolic",
        path: "M8 2a6 6 0 1 0 6 6h-2A4 4 0 1 1 8 4z",
    },
    SymbolicIcon {
        name: "user-bookmarks-symbolic",
        path: "M4 2h8v12l-4-3-4 3z",
    },
    SymbolicIcon {
        name: "view-fullscreen-symbolic",
        path: "M2 2h5v2H4v3H2zm7 0h5v5h-2V4H9zm3 7h2v5H9v-2h3zM2 9h2v3h3v2H2z",
    },
    SymbolicIcon {
        name: "view-restore-symbolic",
        path: "M2 7h7v7H7v-3.6l-3.6 3.6L2 12.6 5.6 9H2zm12 2H7V2h2v3.6L12.6 2 14 3.4 10.4 7H14z",
    },
    SymbolicIcon {
        name: "zoom-in-symbolic",
        path: "M6.5 2a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9zM6.5 3.5a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM4.3 5.8h4.4v1.4H4.3zM5.8 4.3h1.4v4.4H5.8zM9.9 8.9 13.9 12.9 12.8 14 8.8 10z",
    },
    SymbolicIcon {
        name: "zoom-out-symbolic",
        path: "M6.5 2a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9zM6.5 3.5a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM4.3 5.8h4.4v1.4H4.3zM9.9 8.9 13.9 12.9 12.8 14 8.8 10z",
    },
];

const ADD_PATH: &str = "M7 2h2v5h5v2H9v5H7V9H2V7h5z";
const X_PATH: &str =
    "M3.2 2.1 8 6.9l4.8-4.8 1.1 1.1L9.1 8l4.8 4.8-1.1 1.1L8 9.1l-4.8 4.8-1.1-1.1L6.9 8 2.1 3.2z";
const GEAR_PATH: &str = "M7 1.5h2l.4 1.6c.4.1.8.3 1.1.5l1.4-.8 1.3 1.7-1.1 1.2c.1.4.2.8.2 1.2s-.1.8-.2 1.2l1.1 1.2-1.3 1.7-1.4-.8c-.3.2-.7.4-1.1.5L9 14.5H7l-.4-1.6c-.4-.1-.8-.3-1.1-.5l-1.4.8-1.3-1.7 1.1-1.2c-.1-.4-.2-.8-.2-1.2s.1-.8.2-1.2L2.8 6.7 4.1 5l1.4.8c.3-.2.7-.4 1.1-.5zM8 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4z";

pub(crate) fn agent_icon_name(agent: &str) -> &'static str {
    AGENT_ICONS
        .iter()
        .find(|icon| icon.agent.eq_ignore_ascii_case(agent))
        .map_or(FALLBACK_AGENT_ICON, |icon| icon.name)
}

pub fn install() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let icon_theme = gtk::IconTheme::for_display(&display);
    let missing_builtin = SYMBOLIC_ICONS
        .iter()
        .any(|icon| !icon_theme.has_icon(icon.name))
        || AGENT_ICONS
            .iter()
            .any(|icon| !icon_theme.has_icon(icon.name))
        || !icon_theme.has_icon(crate::APP_ID);
    if !missing_builtin {
        return;
    }

    for root in candidate_roots() {
        match write_icon_theme(&root) {
            Ok(()) => {
                icon_theme.add_search_path(&root);
                return;
            }
            Err(err) => {
                tracing::warn!(
                    path = %root.display(),
                    error = %err,
                    "could not install fallback icon theme"
                );
            }
        }
    }
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data_dir) = flowmux_config::paths::data_dir() {
        roots.push(data_dir.join("icons"));
    }
    roots.push(std::env::temp_dir().join("flowmux-icons"));
    roots
}

fn write_icon_theme(root: &Path) -> std::io::Result<()> {
    let hicolor = root.join("hicolor");
    write_if_changed(&hicolor.join("index.theme"), INDEX_THEME.as_bytes())?;

    let actions = hicolor.join("scalable").join("actions");
    for icon in SYMBOLIC_ICONS {
        let svg = symbolic_svg(icon.path);
        write_if_changed(&actions.join(format!("{}.svg", icon.name)), svg.as_bytes())?;
    }

    let apps = hicolor.join("scalable").join("apps");
    for icon in AGENT_ICONS {
        write_if_changed(
            &apps.join(format!("{}.svg", icon.name)),
            icon.svg.as_bytes(),
        )?;
    }

    write_if_changed(
        &apps.join(format!("{}.svg", crate::APP_ID)),
        APP_ICON_SVG.as_bytes(),
    )?;

    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read(path) {
        if existing == bytes {
            return Ok(());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

fn symbolic_svg(path: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#2e3436" d="{path}"/></svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn symbolic_icon_names_are_unique_and_named_symbolic() {
        let mut names = BTreeSet::new();
        for icon in SYMBOLIC_ICONS {
            assert!(icon.name.ends_with("-symbolic"));
            assert!(
                names.insert(icon.name),
                "duplicate icon name: {}",
                icon.name
            );
        }
    }

    #[test]
    fn every_known_agent_has_a_distinct_icon() {
        assert_eq!(agent_icon_name("unknown"), FALLBACK_AGENT_ICON);
        let names = flowmux_procmon::KNOWN_AGENT_COMMS
            .iter()
            .map(|agent| {
                let name = agent_icon_name(agent);
                assert_ne!(name, FALLBACK_AGENT_ICON, "missing icon for {agent}");
                assert!(!name.ends_with("-symbolic"));
                assert_eq!(name, agent_icon_name(&agent.to_ascii_uppercase()));
                name
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(names.len(), flowmux_procmon::KNOWN_AGENT_COMMS.len());
    }

    #[test]
    fn antigravity_icon_uses_its_native_color_palette() {
        let svg = AGENT_ICONS
            .iter()
            .find(|icon| icon.agent == "antigravity")
            .unwrap()
            .svg;
        for color in ["#FFE432", "#FC413D", "#00B95C", "#3186FF"] {
            assert!(svg.contains(color), "missing Antigravity color {color}");
        }
    }

    #[test]
    fn write_icon_theme_creates_index_actions_and_app_icon() {
        let tmp = tempfile::tempdir().unwrap();
        write_icon_theme(tmp.path()).unwrap();

        assert!(tmp.path().join("hicolor/index.theme").exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/window-close-symbolic.svg")
            .exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/vcs-branch-symbolic.svg")
            .exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/flowmux-split-right-symbolic.svg")
            .exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/flowmux-split-down-symbolic.svg")
            .exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/view-fullscreen-symbolic.svg")
            .exists());
        assert!(tmp
            .path()
            .join("hicolor/scalable/actions/view-restore-symbolic.svg")
            .exists());
        for icon in AGENT_ICONS {
            assert!(tmp
                .path()
                .join(format!("hicolor/scalable/apps/{}.svg", icon.name))
                .exists());
        }
        assert!(tmp
            .path()
            .join(format!("hicolor/scalable/apps/{}.svg", crate::APP_ID))
            .exists());
    }
}
