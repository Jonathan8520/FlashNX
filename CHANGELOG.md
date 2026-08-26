# Changelog — FlashNX

Homebrew Flash player for Nintendo Switch (`.nro`), powered by [Ruffle](https://github.com/ruffle-rs/ruffle).

## v1.8.0 (2026-08-26)

Games you can file away, a controls screen that looks like the controller in your hands, and the single biggest speed win FlashNX has had: the memory allocator was eating more than half of every frame.

### Added

- **Folders** (#68): the PLAY screen gets a shelf selector on ZL and ZR. Filing a game into a folder does not move it: the shelf is a label, kept in one small `tags.json` beside the library, which is what lets a game sit in MARIO and in FINISHED at once without being copied anywhere. Folders you made yourself on the card are shown as shelves too, and a game sitting in one of those is on that shelf because of where its file is, so a label cannot take it off; sending it back to ACCUEIL is the one action that does move a file, back to the root. Its save, its controls and its cover follow it when it does.
- **The controls screen is a picture of your controller** (#55, #57): it used to be a list showing eight of the twenty-five buttons at a time, so reading one keymap meant four screens, times five combo layers, times two players. It is now the pad itself, all twenty-five bindings on one screen, with the value of each control beside it. Putting the cursor on a row lights up the matching button on the drawing, so "which one is ZL again" is answered by pointing at it. Left and Right cross between the two hands, the sticks show which way a binding pushes them, and Minus is drawn locked, which finally explains why it cannot be mapped: it opens the pause menu. A button used as a combo modifier is now shown as one, greyed, because the runtime mutes its own key everywhere the moment its layer gets a binding. That was true before and nothing said so.
- **Free zoom on the game picture** (#101): the SCREEN panel gains a ZOOM row. The picture can be enlarged up to five times and moved around, so a game that draws its playfield in a corner can be brought up to the whole screen. The pointer and the touchscreen follow, and the framing is kept per game like the display mode and the filter.
- **Choose where your library lives** (#79): SETTINGS gains a GAMES FOLDER row that browses the card one level at a time, and can make a folder as you go. The folder you pick both receives downloads and gets scanned, and choosing it moves everything there, after a confirmation naming how many games and where they are going. There is deliberately no "leave them where they are": a library split across two places is the one outcome nobody wants. Asked for by players who keep everything under `roms/`.
- **Chinese, Japanese and Korean can be typed** (#75): the system keyboard now offers every input method wherever FlashNX asks for a name, rather than the four Latin layouts it used to be limited to. Address prompts stay Latin on purpose, since an address composed in an input method would not reach anything. and the launcher draws them from the console's own font, so a game named in Chinese shows its real name instead of a row of blanks.
- **A per-game overclock** (#82, #104): the SCREEN panel gains an OVERCLOCK row that raises the CPU speed for that game only. Measured across a range of games it is worth 1.41x on average, from 1.07x on a game that was never CPU bound to 1.75x on one that was. The battery is watched: the setting steps back down on its own below 7%, and the console's skin temperature settled around 44 C over 21 minutes, peaking at 45.3, against the 53 C where the system pins the fan at 60%.
- **Stage3D games run** (#88): the 3D layer is compiled to the console's own shaders and composited with the rest of the picture.
- **Video plays** (#89): Ruffle's software decoder is wired in, so games that embed video no longer sit on a blank frame.
- **The import tab leads with Flashpoint**: searching a game by name and picking it from a wall of covers is the path that needs no address at all, and it used to be one letter in the legend at the bottom while the whole screen said "paste a URL". It is now the first row, above adding an address, and the page is split in two: what you can do, then the sources you have saved.
- **Bug reports carry the technical log**: the end of the session log is attached to a report. Ruffle's own warnings go through the same funnel as ours, so that tail is exactly the list of what the game complained about, which is what tells a freeze apart from a crash. The list of games to report also starts with the one you played last (#99).
- **The current view is named in the home header** (#98), and **a mappable button opens the console keyboard in a game** (#99).
- **Touch reaches the rest of the interface**: the PLAY / IMPORT / SETTINGS bar answers a finger, lists can be dragged, and the cursor glides between rows instead of jumping.

### Fixes

- **Games run close to twice as fast** (#82, #104): the C library's memory allocator was taking 58% of every frame. Flash games allocate constantly, in small pieces, and every one of those went through a routine that walks a list. A cache for small blocks was added in front of it, worth 9.1 fps to 17.4 on the same scene, and it also removed the periodic stall that made a smooth game hitch every few seconds. That stall had been blamed on the garbage collector for months; the collector was only the thing calling the allocator most often.
- **The Chinese interface no longer freezes for two seconds** every time you open it or come back from a game. The console's shared font holds 28 944 characters and the old parser unfolded all of them to draw the hundred an interface uses, which cost 1942 milliseconds and 136 MB, paid again after every game. It now reads only the characters it needs, and the same parse measures 0 ms. Chinese also works again when FlashNX is started from the album rather than as a full application, which used to crash the console outright.
- **Peggle starts** (#100): it is a portal game. The movie tells the page hosting it that it is ready and waits for that page to start it, so with no page it hid everything and waited forever. FlashNX now holds up that side of the conversation.
- **Spellseeker and Hasee Bounce get past their loading screen** (#85, #86): both were missing a companion file that does exist. One was archived under a different host than the one the game asks, so a failed fetch is now retried against `www.` and the bare domain; the other wanted a PHP script the archive cannot keep.
- **A game made of thousands of small sprites is no longer cut off** (#102): The King of Fighters Wing asks for 5137 bitmaps of 384x224 each, and our own guard refused it partway through, which showed as a white screen with a few sprites on it. That guard was written for the opposite shape of game, one that dies on a single large allocation, and one ceiling could not serve both. Ruffle reported the refusal as the graphics device being too small, which sent the search in the wrong direction entirely.
- **Zuma Highspeed Challenge takes touches where you touch** (#87), and pausing a game no longer lets its clock run on underneath.
- **A network error says what went wrong instead of blaming your WiFi** (#103): the message was reached only after the three failures that actually mean "no connection" had been handled, so it accused the one thing the code had just ruled out. It also asked you to check an address, in a search where you typed a game's name.
- **Your nickname can be changed after moving your library**: it was being written to the old location and read from the new one, so the change quietly did nothing. Clearing it now clears it everywhere, instead of letting an older copy come back on its own.
- **Restarting a game from the pause menu no longer freezes its clock** for as long as the menu was open, which left animations stopped in games that time themselves.
- **A game with several animated surfaces updates all of them**: only the last one written in a frame was reaching the screen, so a tile, a gauge or a particle canvas could quietly stop moving.
- **A game name that is not plain English no longer crashes the launcher** (#75), and long names wrap where they actually end rather than where a letter count guesses.
- **Covers from Flashpoint that are WebP under a .png name are decoded** instead of showing as a blank tile, and a transparent cover is no longer drawn as a white block.
- **A failed remote import no longer freezes the interface**, and a download that fails does so in the time of a round trip instead of appearing to hang (#76).
- **The suggestion button says a suggestion was sent** (#83), not a bug report.
- **A HOME menu shortcut applies the defaults you set in SETTINGS**, which it was ignoring.

## v1.7.0 (2026-08-10)

Four ways to lay out your library, a game that can fill the screen or be turned upright, and a long list of things that used to claim they had worked when they had not.

### Added

- **The PLAY screen can be laid out four different ways** (#52): SETTINGS now starts with a HOME VIEW row. GRID is the cover grid you already know and is still the default; the other three exist because a grid has to crop every cover into the same box, and a lot of Flash game art is a wide banner or a small logo that does not survive that crop. LIST is a column of thirteen titles with the selected game's cover shown whole beside it, which is the only layout that gets you through a long library by reading rather than by looking. STRIP puts a large cover on the left, its details on the right, and your whole library in one row along the bottom, about five covers at a time. SHELF is a single row of larger covers with the current one grown in place. Your choice is kept for the next time you start FlashNX. Favourites keep their gold marker everywhere, every cover has rounded corners, and in the two row layouts you can drag the row with a finger, tap a game to select it and tap again to launch it. Up and Down hop four covers at a time along a row, and Left and Right skip five titles in LIST.
- **A game can now fill the screen instead of sitting between black bars** (#65, #69, #74): Flash games were drawn for a computer monitor, most of them in 4:3 and often at 640x480, while the Switch screen is 16:9, which is why the launcher looked fullscreen and the game did not. The pause menu (`-`) has a new SCREEN row between CONTROLS and RESTART, and the first line inside it, DISPLAY, cycles between three ways of using the screen. The frozen picture behind the panel is redrawn at each press, so you see what a mode does to that particular game before going back to playing. FIT is the default and is what Flash itself does: proportions kept, black bars where the game does not reach. STRETCH fills the screen by widening the picture, so it distorts, and it comes first in the cycle because it is the only mode that still shows all of the game. FILL keeps the proportions and enlarges the picture until it covers the screen, cutting off whatever hangs over the edges: on a 4:3 game that is about a quarter of its height, top and bottom, which is exactly where scores and life bars live, and on a tall game like *Flappy Bird* (500 by 700) it would cut about 60% of the playfield. The choice is remembered for that game only, which is the point: the price of filling the screen depends entirely on the game.
- **Screen filters, for an old-television look** (#65): the last line of that SCREEN panel, FILTER, offers NONE, SCANLINES or CRT. SCANLINES darkens every other line of the picture; CRT adds a colour stripe mask and a soft darkening towards the corners on top of the lines. The change shows on the paused picture straight away and is remembered per game. The lines are spaced on the game's own resolution rather than on the console's screen, because one dark line per screen pixel is finer than the eye can see and only makes the picture dimmer. The image is deliberately not curved: the pointer and the touchscreen land exactly where the picture is drawn, and bending it would put what you see and what you tap in two different places. The filter covers the game only, so the pointer, the pause panel and the menus stay clean. With it off, nothing is drawn differently and nothing extra is loaded, but on a game that is already struggling a filter is one more thing to draw.
- **Defaults for the whole library, so you do not have to set every game one by one** (#65, #69, #74): wanting to lose the black bars usually means losing them everywhere, and doing that game by game means doing it dozens of times. The cursor-speed row in SETTINGS becomes DEFAULT SETTINGS and opens a panel with four lines: the default controls (which used to be the first row of SETTINGS), the display mode, the screen filter and the cursor speed, each showing its value and cycling in place. A game you have never touched from its own pause menu follows these defaults; a game you have set keeps its own choice. The honest part: it keeps it for good, so a game you set once no longer follows a later change of the default, and there is no button that puts it back to following it.
- **You can look at a screenshot of the game instead of its logo when picking a cover** (#59): in a game's options, CHOOSE A COVER has a new `Y` prompt that swaps the whole grid of results between logos and screenshots. Screenshots are always game shaped, measured between 1.4:1 and 1.7:1 across the library, so they fill a tile as they are, while logos come in any shape at all: *QWOP*'s is a 640 by 76 banner that loses almost everything to the tile crop, and *Hot Dog Bush*'s is a tall badge. Which one reads better depends on the game, so nothing is imposed: logos stay the default everywhere (*Bloxorz*'s is excellent as it is), the swap applies to the one cover you are choosing, and the picker opens on logos again next time. When a game only has one of the two images, the other one is fetched instead of leaving the tile blank.
- **Games imported from archive.org now bring their data files with them** (#73): a game's levels, dialogue or settings are sometimes kept in a separate file next to it, and a game that cannot find that file does not fail, it waits. *Battle for Dream Island 5b* sat on an empty green screen at a perfectly steady 60 fps forever, having asked once for its level table and been refused. FlashNX already looked for companion games, but not for plain data files, so a game like that looked self-contained. It now reads the file names the game itself mentions and downloads the ones the archive.org item holds into the game's own folder, so *Battle for Dream Island 5b* gets its 64 KB level table. Only the names the game asks for are fetched, never the whole item, which is mostly archive.org's own bookkeeping. And when the game names a file nobody has, the message at the end of the import turns red and names it, instead of leaving you to work that out from a game that loads forever.
- **A game can be turned a quarter turn at a time** (#78): the middle line of the SCREEN panel, ROTATION, turns the picture by 90, 180 or 270 degrees. Scaling cannot help a game drawn taller than it is wide: *Flappy Bird* is 500 by 700, so on a 16:9 screen it either sits in a narrow column between two large black areas or loses most of its playfield to a crop. Turned a quarter and played with the console held upright, it uses the whole screen at its own proportions. The pointer and the touchscreen turn with the picture, so what you aim at is still what you touch, and the choice is kept for that game alone, like the display mode and the filter. The launcher itself never turns, whatever a game is set to.
- **A game can be turned a quarter turn at a time** (#78): the middle line of the SCREEN panel, ROTATION, turns the picture by 90, 180 or 270 degrees. Scaling cannot help a game that was drawn taller than it is wide: *Flappy Bird* is 500 by 700, so on a 16:9 screen it either sits in a narrow column between two large black areas or loses most of its playfield to a crop. Turned a quarter and played with the console held upright, it uses the whole screen at its own proportions. The pointer and the touchscreen follow the picture round, so what you aim at is still what you touch, and the choice is kept per game like the display mode and the filter. The launcher itself never turns.

### Fixes

- **Picking a game whose file is no longer there does not start a different game.** If the chosen file could not be read (deleted, renamed, or damaged), FlashNX quietly worked down a built-in list of file names and launched the first one that answered: another game, running with that game's controls and its saved games, while your time played was credited to the game you had picked. Nothing anywhere said so. It now stops on the game you chose and shows the plain red fallback screen, which you can back out of. The easiest way to hit this was a HOME menu shortcut, since it points at one fixed file and keeps pointing at it after the game is moved or deleted.
- **Your time played and your favourites can no longer be erased by your own click.** Both are kept in a file on the card. If that file could not be read at startup, the app carried on with an empty list and the next write replaced the file with it. One star made every other favourite disappear, and quitting a single game replaced every recorded hour with that one session, so every game showed 00:00 and MOST PLAYED and LAST PLAYED went alphabetical. It looked exactly as if your own click had wiped the lot. A file that cannot be read is now left untouched for the rest of the session instead of being overwritten. A first start with no file at all is still a normal empty start.
- **Two unrelated games no longer share one save file.** Your progress was filed under the name a game's main file had on the website it originally came from, not the name it has on your SD card, and a great many Flash games ship a main file with a name so generic that hundreds of them use the same one: across the Flashpoint catalogue, 5.3% of entries are in that situation. Two such games wrote to the same save and each wiped the other's progress. Saves are now filed under the game's own name on the card. Nothing is lost in the update: an existing save is still read, and the first time a game saves it writes itself out under the new name, so you carry on where you were. Because old saves are still read, a game that has not saved once since the update can still be handed the other game's file the one time it looks, and progress that two games had already mixed together stays mixed; it sorts itself out as soon as each game saves once, which normally happens the first time you play it. A small note file appears next to each game, recording which save it opened.
- **Deleting a game now removes its saves too.** Delete looks for files named after the game, and old saves were named after the site the game came from, so they survived: *Agent P Strikes Back* kept its save through a delete and picked it straight back up when reinstalled. An old name cannot be worked out afterwards without risking another game's progress, so FlashNX now notes which save a game opens, at the one moment the link is certain, and deletes exactly those files, never a name it guessed at. Because that note is only taken when a game is played, launch a game once after this update if you want deleting it to leave nothing behind.
- **A download that did not bring back a game no longer passes for a success** (#73): if the address you saved carried anything after a `#`, the list of files came back correct and nothing looked wrong, but the download itself asked archive.org for the wrong thing and got the item's own web page instead: 148 KB of page, saved under the game's name and announced as a finished download. The game then never appeared in your library and nothing said why, so the same import could be retried four times in a row with the same silent result. Addresses are now cleaned before anything is fetched, and every finished download, from a URL or from Flashpoint, is checked to actually open as a game before it is kept and before the green confirmation and the OK badge appear. If it does not, the file is deleted, the error screen names the cause, and `Y` lets you correct the address on the spot. The same check catches a dead mirror answering with an error page, a redirect landing on something else, and a transfer cut short.
- **Some Flashpoint games were starting an advert instead of the game** (#72): when the archived copy of a game does not contain the exact file its Flashpoint entry points to, FlashNX has to choose one itself, and it used to take whichever came first in the archive. For *Hot Dog Bush* that is a 4.8 KB in-game ad frame, so the advert is what booted: it played its banner, asked a host that has been dead since 2020 for an image, then sat there forever, because there is no game behind an ad frame. The choice is now ranked instead of taken in storage order: first a file with the same name at a different address (the game simply moved), then the largest game file that does not come from a known advertising service. *Hot Dog Bush* now loads its real 3 MB game, and whether it then plays well is another matter. If an advert is the only thing in the archive it is still used, exactly as before.
- **A cover that fails to download no longer leaves the game with a plain generated tile for good.** FlashNX checked only that the server had replied, not that the reply was a picture, so a mirror's error page or a transfer cut short was saved as the game's cover all the same. The file then existed, so that game showed the generic cover from then on while the app reported the cover as saved, and trying again did exactly the same thing. The image is now decoded before it is written: the cover picker shows you the failure, nothing bad is kept on the card, and a later attempt can succeed. This applies as well to the art fetched automatically when you import a game from Flashpoint.
- **Typing the address of a game you already have now tells you so.** It used to do nothing at all: no row, no message, no error, so there was no way to tell "you already have this one" from "the button did not register", or from another game that happens to use the same file name. The address is also added to your saved URL list, which used to drop it in silence.
- **A cancelled multi-file download no longer follows you into the next one.** Cancelling a game while its companion files were being fetched left the queue armed, so the next game you downloaded inherited it, wrote those leftover files into its own folder and counted its progress on from wherever the abandoned download had stopped.
- **"Your previous controls were saved" is now verified before it is said** (#20): applying a shared profile copies your own controls aside first, and that copy is exactly what fails on a nearly full card, while the profile it protects still applies. Nothing checked it, so the menu went on offering to put your own controls back, and choosing it wrote the empty copy over the controls you had set by hand. The backup is now read back before it is offered as one, a copy that failed halfway is removed instead of being left to look like a restore point, and a revert that could not go through says so instead of "your controls were restored".
- **Applying a shared profile no longer lets you re-publish it under your own name** (#20): after applying a profile that also carries a pointer speed, SHARE could offer to send it back to the catalog as this console's own work. It cannot now. Profiles without a pointer speed were never affected.
- **A catalog that could not be reached is no longer announced as an empty one** (#20): offline, with a wrong console clock (which genuinely breaks secure connections) or with the server having a bad moment, the profile picker said "NO PROFILE FOR THIS GAME YET. SHARE YOURS TO HELP!", a statement about other players' work that nothing had checked, followed by an invitation to act on it. It now says the catalog is unavailable and to check your connection, in all nine languages, on both the in-game and the library screen. A catalog that arrived unreadable was also remembered as an empty one for the rest of the session; it is not any more.
- **A game with a lot of shared control profiles no longer hides the first of them** (#20): the list sized itself to the number of entries and is centred, with no scrolling, so past ten profiles for the same game it was taller than the screen and its top rows were drawn off the edge. It is capped at ten, as the preview already was.
- **A change the SD card refused no longer looks like it worked.** Remapping a button showed the new key in the list even when the card would not take the change, so the list displayed a key the game does not use and the old mapping was back at the next start. Renaming a game did the same with the new name. And "leave empty to restore the original name" reported success without checking that anything had actually been removed, so the old name came back at the next scan. All three now report the SD card error and leave things as they were. The same silence affected the DISPLAY, FILTER and POINTER SPEED rows, where it did not look like an error at all but like a dead button: the row is relabelled by re-reading the file that was never written, so it never moved, and pressing again re-applied the very same value.
- **The IMPORT tab's counter and its OK marks say the same thing.** Opening a large archive.org item could show "2 / 81" on the row while a dozen lines in the list below were ticked as already on your card. The two were answering different questions: a line is ticked when a game of that name is on the SD card, while the counter was counting games recorded as having come from that item. Pressing `A` on a file already refuses to download it whenever a game of that name exists, so the name is what the app treats as identity everywhere else, and the counter now counts exactly the ticked lines. It is worked out when the item's list of files is opened and goes up with each download, so the figure stays right without leaving and reopening the item.
- **Starting up got quicker again.** Answering the old counter's question meant opening one extra small note file for every game in your library each time FlashNX scanned the card, and that turned out to be the single most expensive step of the scan: about 0.2 seconds of it on a 79-game library. Nothing needs it at scan time any more, so the whole pass is gone. The note itself is still written, and bug reports still use it.
- **Every list wraps round, and Left and Right skip five rows in all of them.** Reaching the last game and carrying on brings you back to the first, and the other way round, which the saved-URL list already did and nothing else did. A move stops at the edge first and only the next press crosses over, so a skip near the end of a long list cannot fling you back into the middle of it. A held direction stops there for good and only a new press comes round, because wrapping and hold-to-repeat together made a carousel: holding Down ran off the bottom, reappeared at the top and kept going, so the one thing holding a direction is for, reaching the end of a long library, was the one thing it could not do. Left and Right did nothing at all in the two IMPORT lists and now skip there too, the same five rows they skip in the home list: inside an item that sits between the single row of Up and Down and the full screenful of `L` and `R`. Holding the stick diagonally no longer fires the skip and the single step at once, which moved five games for one flick of the thumb.
- **A long game name is now shortened in the middle rather than at the end.** These are file names, and what tells two of them apart sits at the end at least as often as at the start: the four *Scooby-Doo: Mayan Monster Mayhem Episode N* entries were cut before the episode number and drawn as four identical rows.
- **A search that matches nothing now says so.** The screen used to draw its frame around nothing at all, with only the "0 / 71" in the header to hint at what had happened. All four layouts now print NO RESULTS in the middle of the empty area.
- **A game's details read the same way on every screen, and the header takes up less room.** The grid built its own line with double slashes and its own spelling of the same facts, so one game read two ways on two screens. There is one line everywhere now: size, then the Flash version and compression, then the engine, then your playtime when there is any, separated by thin vertical bars. Playtime appears once there is at least a minute on the clock and reads as 1H05. At the top of PLAY, the logo is drawn smaller and the games count has moved up beside it instead of sitting alone on a full row underneath, which gives back about a third of the header's height; a long search term is now cut short rather than running over the logo. The space freed is left as clearance, so no layout gains an extra row of games from it.
- **Learn to Fly 2 starts** (#76): it opened on a black screen and stayed there. The game leaves its first frame only through a jump fired by an advertising callback, and that callback fails here in no time at all, so the jump was asked for while one frame of forty-six had loaded. A jump past what is loaded is quietly dropped, and the game had already stopped itself, so it parked on frame one for ever with nothing said. Against a real advertising server the reply takes long enough that the game has moved on and skips the jump by itself, which is why the same file plays everywhere else. FlashNX now finishes loading a game before running it, for anything under 64 MB. The visible side of that: a game's own loading animation no longer plays, since by the time the game runs there is nothing left to load. Above 64 MB nothing changes, because there the wait is long enough to be worth watching.
- **The covers no longer vanish when you open the language list.** The list names every language in its own alphabet, so opening it draws Chinese characters even if you are in French, and that emptied the whole gallery: the shared-font text was writing over the shape every picture in the launcher is drawn from. Going back to French did not bring them back, and only launching a game did, which made it look like a language problem. It was not.
- **Chinese menus appear much faster.** Every text was costing one drawing call per character, and every return from a game re-read the console's 7.6 MB Chinese font from scratch, which measured just under two seconds each time. A line of text is now drawn in one call, and rasterised characters are kept for the whole session, so a session with two games went from five of those waits to two. The first one of a session is still there.
- **A second search no longer leaves the launcher with no network at all.** Starting a Flashpoint search while the previous one was still loading returned an error, and that error was the visible half: the refused request left the connection slot held for ever, so every later request in the session failed the same way, searches and cover downloads alike, until you quit. A new search now replaces the one in flight, which is what starting one means anyway. A download in progress is still protected.
- **A game keeps one settings file instead of three.** Display mode, screen filter and rotation each wrote their own small file next to the game, so a library where you had set a few games was littered with them. They share one file now. Nothing is lost: the three all arrived after v1.6.0 and were never in a published release.
- **The game name no longer touches the light under the shelf.** In SHELF, the lit segment under the selected cover grows as it lights up, and at full brightness it reached into the first row of pixels of the title below it. The name and its details also sit lower now, since the page had a large empty area under them.
- **The selected row is rounded in IMPORT and SETTINGS**, as it already was on PLAY.
- **Two French labels regain their accents**, ETAGERE and VERIFIE.

## v1.6.0 (2026-08-04)

More games playable, a faster player, and an import tab that survives a long list of URLs.

### Added

- **Games that are packaged as a web page now run**: several Flashpoint entries do not ship a plain `.swf` but an `index.html` that feeds settings to the player. FlashNX now emulates that container, which brings in Disney/Yamago minigames (*Agent P Strikes Back*, *Tron Uprising: Escape from Argon City*) and HTML-wrapped titles such as *Dragon City*. These games read their settings from a script the browser would have run, so FlashNX now reads that script's configuration itself instead of it being written in by hand for one game.
- **Games that expect to run inside a web page are told so only when they need it**: this used to be claimed for every game at once. Some of these titles behave the opposite way and, believing they were online, waited on a game service that has been shut down for years. Each one now gets the answer that suits it, which is what makes *Tron Uprising: Escape from Argon City* playable and saving its progress.
- **Newgrounds API games load again** (for example *Newgrounds Rumble*): their preloader waited forever on a Newgrounds service that no longer exists, so they sat at "WAIT" and never reached the title screen.
- **Very large games download**: the importer streams a game archive straight to the SD card one file at a time instead of holding it in memory, so multi-gigabyte entries (for example *Super Smash Flash 2*) can be fetched and extracted. Whether such a game then runs well is another matter, but it is no longer blocked at the download step.
- **The IMPORT tab handles a real collection of URLs.** Each row now shows a readable name instead of a truncated URL, with a tag saying whether it is a single `.swf` or a list of files, and a counter of how many of that source's files are already on your SD card. `+ ADD A URL` is pinned to the top so it stays reachable, `-` searches, `Y` sorts (added, name, source, file count), and a URL can be favorited so it pins to the top like a favorited game. The list also scrolls smoothly and can be dragged with a finger.
- **A saved URL's options show what it is**: type, files on the SD card, the date you added it, and the full URL, so you know what you are about to edit or delete.
- **Your library size is shown under the logo** ("71 GAMES"), and it follows the search when one is active.
- **The Flashpoint details popup (`+`) shows the cover and the game's description.**
- **A toast confirms a finished download**, instead of the download screen simply disappearing.
- **The library footer shows a multi-file game's real size on the card**, not the size of its small loader file.
- **Searching Flashpoint is less of a dead end**: `X` re-edits the current search without going back, and dismissing a message returns you to the results instead of the home screen.

### Fixes

- **Three games no longer take the whole app down with them.** *Sonic RPG Episode 10*, *New Super Bowser World* and *haunt-the-house* each ran the console out of memory. None of them is fully playable yet, but none of them can now cost you an unsaved game elsewhere.
- **Network errors say what actually went wrong.** Every failure used to show the same "check the clock, the WiFi and the URL" sentence, including cases that had nothing to do with any of the three. You now get the real cause: no connection, timeout, a search returning too much data, a wrong console clock (which genuinely does break HTTPS), a missing item, or a full SD card. When a URL is at fault, `Y` lets you correct it and retry on the spot.
- **A Newgrounds game still starts on the fourth try.** These games open by talking to a Newgrounds service that shut down years ago, and FlashNX answers for it. That answer has a safety limit, because one game asks again in an endless loop and would otherwise pin the console. The limit was counted for the whole time the app was open instead of for the game being played, so after three launches it was spent, and the next Newgrounds game you started sat on "Connecting to the Newgrounds API Gateway" forever. Each game now gets its own allowance, and the runaway game is still stopped.
- **A Flashpoint game's download size is shown instead of a question mark.** Not every game there is packaged as an archive on the download server; some sit loose on the file mirror, and FlashNX already fetched those from the mirror. The details popup asked the archive server about them all the same, and it has nothing for those games, so their size came back as a question mark even though they downloaded and played normally.
- **Long messages stay on screen.** Some notices were drawn as a single line and ran off both edges, and text in accented or non-Latin languages was wrapped too early.
- **The launcher opens about five times faster.** The black screen before your games appear went from roughly 3.6 seconds to 0.7 on a 71-game library, and the same wait when you quit a game back to the list is cut the same way. The grid was preparing the cover of every single game before it could draw anything, so it now prepares only the tiles you can actually see; the SD card was being asked about companion files that were not there, which is now answered from the directory listing the app already read; each game was reopened on every scan just to re-read the header it had read the last time, which is now remembered until the file itself changes; and the launcher was reserving 576 MB of graphics memory that only a game ever uses.
- **Moving through the game grid is smooth.** Text was drawn as a separate graphics call for every run of pixels in every letter, thousands per frame, which on its own used the entire time available for a frame. A line of text is now drawn in one call. Covers are also resized once to the size they are shown at and kept that way on the SD card, so scrolling into a new row no longer drops frames. Changing a game's cover refreshes it by itself, and the full-size image is still what you see in the launch animation.
- **The pause menu no longer stays stuck small on a slow game.**
- **Faster on script-heavy games**: the code is now built for the Switch's own processor and uses its optimized memory routines. *Dragon City* in particular goes from unplayable to around 6 fps after a graphics setting that was throttling the GPU was corrected.
- **Papa Louie's character is visible again**, and *Papa Louie 3* no longer hangs on its loading screen.
- ***Pursuit of Hat 2* no longer starts on a black screen**, and its invisible platforms are drawn.
- ***Icy Tower* no longer crashes**, and its HUD is complete.
- ***Cat Mario* no longer loses whole groups of sprites in busy scenes**: near the end of the first level the game draws far more elements at once than the rooms before it, and past a certain number of them entire batches stopped appearing, so the character could vanish along with the ground under it. The graphics memory the player recycles from one frame to the next was capped at a fixed size that this scene outgrew, so beyond that point it was thrown away and rebuilt every single frame until the console ran out and gave up on the drawing. It is now released only when something is actually short of it, and even then only the parts no longer in use, so nothing a game is still working with is taken away from it. Nothing is dropped, and the heaviest parts of the level run faster.
- **A drawing a game makes for itself is the one it gets back.** Many games build an image by drawing other images into it, for scenery, for a light or a blur, for a character assembled from pieces. That result was parked in a scratch area the player reuses for the next drawing, and a game is free to read its own image back much later. By then the scratch could have been handed to something else, so the game got another sprite's picture, or an empty one. Every result now lives in the image it belongs to and stays valid however long the game waits, which is what makes *Papa Louie 3* and *Icy Tower* reliable rather than only usually right.
- **A Chinese interface no longer takes the console down** (#70). Chinese is drawn with the console's own font, which needs over 130 MB to load, far more than homebrew gets when it is started from the Album, and loading it there crashed the console outright rather than failing quietly. This hit on the very first run, before you could pick anything, since FlashNX follows the console language until you choose one. Started from the Album, the interface now falls back to English and the language list says why. Started from a HOME menu entry, where FlashNX has the full memory, Chinese works as before.
- **Games with a broken version check no longer refuse to start** with a false "Flash Player required" message (#64).
- **Large `.swf` files load** instead of being rejected for their size.
- **Housekeeping**: empty `.files/` folders are cleaned up and no longer created for nothing, a duplicated copy of a game's main file is no longer left behind, and the one-time first-boot migration is more robust.

## v1.5.1 (2026-07-08)

A small fixes release.

### Fixes

- **Games no longer turn to a red screen after a long session** (#62, #63): after playing one game for a while, launching a different one could show a full red screen. The game file was being read in a single large operation that could spuriously fail once memory had fragmented; it is now read in small chunks like the rest of the app, so the next game loads reliably.
- **Caps Lock can now be mapped to a button** (#61): a few games use the Caps Lock key for a mechanic (for example, a stage in *This Is the Only Level* that only opens the exit while Caps Lock is held). The visual keyboard in the control editor (TOUCHES) now has a **Caps** key you can bind.

## v1.5.0 (2026-07-02)

A rework of the control editor: a visual keyboard, more keys, button combos, and per-game cursor options.

### Added

- **Visual keyboard for remapping** (#55): the control editor now shows a full PC keyboard to pick a key, instead of a long scrolling list. Navigate it with the D-pad or stick and press **A**. Ctrl, Alt, Tab and the like are now easy to find, and the board covers the whole layout. Keys already used by the current mapping are highlighted, so you can see at a glance which ones are taken.
- **More keys**: F1-F12, the symbol keys (`-` `=` `[` `]` `;` `'` `,` `.` `/` `\` `` ` ``) and the numpad operators (`+` `-` `*` `/`) can now be bound.
- **Button combos, one layer per modifier** (#57): for games that need more inputs than the controller has buttons, hold a modifier button and every other button sends a different key, so `ZL + A` can send `F1`. Each of the four modifiers (ZL, ZR, L, R) has its **own** combo layer and they all work at once in a game, so `ZL + A` and `R + A` can be different keys. The editor has a sub-tab (move with **L/R**) across `NORMAL / ZL / ZR / L / R`: NORMAL is the base bindings, and picking a modifier edits that modifier's own layer (rows then read `ZL+A`). **X** switches the Player 1 / Player 2 tab, each with its own layers. A button with no combo key keeps its normal one while a modifier is held, so movement never breaks.
- **Show or hide the mouse cursor per game**: a toggle in a game's controls options (TOUCHES) hides the on-screen pointer for games played with the pad or keyboard where it just gets in the way. Clicks still work, only the pointer is hidden.
- **Shared profiles now carry the whole setup** (#20): sharing or applying a community control profile also transfers the combo layers, the cursor speed, and the show-cursor choice, not just the base bindings. The before/after preview shows these too.

### Fixes

- **Very large games no longer go to a white screen** (#56): games with thousands of unique vector shapes (e.g. the Henry Stickmin titles like *Infiltrating the Airship*) filled the shape buffer partway in, after which the rest of the art stopped drawing. The buffer is now large enough to hold these, so they render fully.
- **Fewer white screens / out-of-memory on heavy games** (e.g. *Super Bowser World*): the dedicated bitmap layers are now freed as soon as they empty and sized to what they actually hold, instead of piling up and exhausting memory.
- **Water and distortion effects now render correctly**: games that ripple graphics with a displacement-map filter (e.g. underwater levels) used to show garbled stripes, or the effect did nothing. The filter is now supported, and a texture-packing bug that striped these scenes is fixed.
- **A game no longer appears twice in the shared-profile catalog**: the `.swf` file extension is no longer part of the title used to match profiles, so entries for the same game line up instead of splitting (this was showing *Super Mario 63* twice).
- **The on-screen cursor is easier to see**: the pointer now has a black outline so it stays visible over both light and dark game art.
- **Changing only the cursor speed re-enables sharing**: after applying a community profile, adjusting just the pointer speed used to still say there was nothing to share; that change now counts.

## v1.4.1 (2026-06-28)

A follow-up to the Chinese support in v1.4.0, plus wider import support.

### Fixes

- **Chinese (and Japanese/Korean) text now shows inside games**: v1.4.0 added Chinese to the app's own menus, but text drawn by a running game still came out blank when the game used a system font for it. Games now fall back to the Switch's built-in fonts for any character a game's font is missing, so in-game CJK text renders. (#54)
- **Import from Wayback Machine links**: a `web.archive.org` snapshot URL of a `.swf` is now accepted and downloads the actual game (it used to be treated as an archive.org item and fail).

### Changed

- The control-profile catalog is now fully community-driven: the one bundled profile (Super Mario 63) was removed, as it only mirrored the default controls anyway. Share and apply profiles from a game's options as before.

## v1.4.0 (2026-06-25)

Community control profiles, more languages, and Flashpoint games that were impossible to import before.

### Added

- **Community control profiles**: share your key bindings for a game and download other players' setups (#20). In a game's options (**+**), pick "Share my controls" to publish your profile, or "Apply a profile" to browse what the community has shared for that game and try it; applying a profile is non-destructive and can be reverted. Profiles you shared can be deleted again. Verified and most-applied profiles sort to the top.
- **More languages**: German, Italian, and Brazilian Portuguese join the menus, plus **Simplified Chinese** (#41) rendered from the Switch's own shared font.
- **Numpad keys in the controls editor**: the editor now offers Num0–Num9 (the numeric keypad), listed first, for games that read keypad keys separately from the top-row digits. Player 2 defaults to the numpad.

### Fixes

- **Flashpoint games with a non-ASCII title now import and launch**: a game whose file name uses non-Latin characters (for example *包丁少女幻窓曲*) failed to download with an error -2. Its address is now encoded correctly. (#51)
- **Flashpoint games that load their assets on the fly now play**: some games build the paths to their data and art files while running, so those files could not be fetched ahead of time and the game stayed on a blank screen (for example *Racing is Magic*). Missing files are now pulled from the Flashpoint mirror on demand and cached, for games imported from the Flashpoint search. (#51)

## v1.3.1 (2026-06-19)

Local two-player, a touch-driven launcher, and a batch of game fixes.

### Added

- **Local two-player (two controllers)**: a second controller now drives Player 2 through its own set of key bindings, for Flash games where two players share one keyboard (for example *Fireboy & Watergirl*, *Dragon Ball Z Devolution*). The controls editor has a Player 1 / Player 2 toggle (press X), and both players' keys are saved per game. Player 2 defaults to WASD so it does not clash with Player 1's arrows. Needs two full controllers (a Pro controller or a Joy-Con pair each). (#40)
- **Touch controls in the launcher**: in handheld mode you can drag the game gallery to scroll, tap a game to select it, and tap it again to launch it.
- **Flashpoint content filter toggle**: press ZL+ZR in the Flashpoint search results to turn the content filter on or off. Importing a game also fetches its cover automatically. (#33)
- **Download of non-zipped Flashpoint games**: games served loose (not as a single archive) now download through the htdocs mirror. (#26)

### Fixes

- **Super Smash Flash**: the announcer now plays, and the game no longer freezes on a blank screen after a fight instead of showing COMPLETE. Its voices and most of its sound effects use the Nellymoser audio format, which was not enabled. (#29)
- **No more crash when some games save**: a game saving a self-referential object (for example *Hemp Tycoon*) used to crash the app. The save now completes. (#33)
- **Color speckle on translucent effects fixed**: semi-transparent effects (for example the smoke in *Offroaders*) showed cyan and magenta speckle. (#38)
- **Flashpoint games with a space in their name now launch the right file** instead of the first one found in the archive.
- **The "&" character now shows in the menus**: it was missing from the UI font, so titles like "Fireboy & Watergirl" dropped it.

## v1.3.0 (2026-06-14)

A big one: multi-file games, Flashpoint downloads that actually start, an in-game keyboard, favorites, and a much more configurable control editor.

### Added

- **Multi-file game support**: a game that loads other `.swf` files at runtime (`loadMovie` / `loadMovieNum` into a level) now finds them in a `<game>.files/` folder next to the `.swf`. Download a game from the Flashpoint search (X) and its companion files are fetched automatically; for a game added another way, drop the companions in that folder yourself. *Garfield's Scary Scavenger Hunt* now plays from start to finish.
- **Multi-file indicator**: the launch screen shows a "MULTI-FILE (N)" label when a game pulls in companion files, so you can tell at a glance.
- **Flashpoint downloads now bring the whole game**: a download from the Flashpoint search now unpacks the game's full bundled set of files (alternate versions, ad-network stubs, data files) and launches the exact version the archive intends, instead of guessing. Games that used to get stuck on a sponsor or "Download the latest Adobe Flash Player" screen (for example *Papa Louie 2: When Burgers Attack*) now start and play.
- **In-game keyboard**: when a Flash game wants text (a player name, a level password, high-score initials, a text adventure), the Switch keyboard opens when you click the text field, pre-filled with its current text and set to the right type (numbers, password, multi-line). Games that were unplayable with a controller alone now work.
- **Favorites**: in a game's options (**+**), mark it as a favorite. Favorites are pinned to the top of the Play gallery with a gold marker, whatever the sort order.
- **Assignable mouse clicks**: the controls editor now has **Left click** and **Right click** actions you can bind to any button. By default **ZR** is left click and **ZL** is right click. The touchscreen still left-clicks.
- **More mappable inputs**: SL / SR (Joy-Con side buttons), the stick presses (L3 / R3), and the **right stick as a d-pad** (bind its directions and it stops being the mouse cursor; the touchscreen stays the cursor).
- **Translated control labels**: the keys shown in the controls editor (clicks, Space, Enter, arrows, and so on) now follow your language (English, French, Spanish, Russian).
- **Adjustable cursor speed**: the right-stick mouse cursor now has a speed setting (x0.5 to x2.5), in Settings and in the in-game pause menu where it cycles live as you press it. Handy for games that need fast mouse movement (for example *Spank the Monkey*). Your choice is saved across games and launches. (#17)
- **Much faster Flashpoint downloads**: downloads now batch their writes to the SD card and pump the network harder, turning what used to be a roughly two-minute download into about ten seconds for a large game.
- **Home-menu shortcuts for a single game**: FlashNX can now be launched straight into one game when its `.swf` path is passed as a launch argument — it skips the library and returns to the Home menu when you quit. With a homebrew forwarder tool this lets you put a specific Flash game on your Switch Home menu, with its own cover as the icon. If you use **Sphaira**, FlashNX registers a `.swf` association on launch, so you can pick a `.swf` in its file browser, choose "Create a Forwarder", select FlashNX, and the shortcut boots straight into that game.

### Fixes

- **Buttons mapped to letter or number keys now trigger games that read them as keyboard shortcuts**: a controller button bound to a letter (or a digit or space) now fires a game's keyboard shortcuts, not just its held-key checks. For example *Scooby-Doo: Mayan Monster Mayhem* (H for help, S/T to switch the held item) now responds; before, only movement and pickup worked.
- **Deleting a game also removes its `<game>.files/` companion folder and its favorite mark**, so nothing is left behind on the SD card.
- **Games that rendered as a blank white screen now display correctly**: very art-heavy games (thousands of on-screen vector shapes at once, for example *The Binding of Isaac*) exhausted the renderer's geometry buffers, which silently dropped the extra shapes and left most of the game invisible behind a few stray text glyphs. The buffers are now much larger, and a buffer overflow is reported instead of failing silently. (#16, #23)
- **Large multi-file games now download and run**: big Flashpoint games (over the old 64 MB download limit, loading dozens of companion files by relative path, for example *Super Brawl 2*) used to show a full download bar and then do nothing, or launch to a black screen. They now extract fully and run. This took a few fixes: a larger download limit, launching the game under its original URL so its relative asset loads resolve, and doing the extraction and asset reads through the C++ filesystem layer (the Rust one drops some files on the Switch).

## v1.2.1 (2026-06-11)

Small fix release: games that use PixelBender shaders no longer crash.

### Fixes

- **PixelBender games no longer crash**: some games build a Flash `Shader` / `ShaderFilter` at runtime (for example **The Terminal**). They used to abort the app the moment the shader was created. They now run normally; the shader's visual effect itself is skipped (this renderer does not run PixelBender), but gameplay and input work. As a bonus, crash messages from the game thread are now captured to the crash log instead of being lost.

### Changes

- **Clearer bug reports**: a report now includes the game's import URL when it was added from a link, so a game imported under an arbitrary filename can still be identified. The report also reminds you that it opens a public issue on the FlashNX repository, and you can add your GitHub handle if you want a follow-up.

## v1.2.0 — 2026-06-10

Big library update: a tabbed navbar, a cover-art gallery, a list-based importer, Flashpoint game downloads, library sorting, playtime, and in-app bug reports.

### Features

- **Tabbed navigation**: a top navbar switched with **L / R** between **Play** (your games), **Import**, and **Settings**.
- **Cover gallery**: the Play tab is a grid of cover art, 5 per row (covers are cropped to fill the tile). Games with no cover get a generated tile (color + initials).
- **Your own covers**: drop a `<game>.png` or `.jpg` next to the `.swf` and it shows up as the cover.
- **Flashpoint covers**: a game's options has a **Cover** action that searches the Flashpoint Archive by name and shows the candidates as thumbnails to pick from. The search name is cleaned up automatically (download-id suffixes such as `game-15938d603` are dropped), and **−** lets you retype the title when the filename does not match the catalog (for example `catmario` to `cat mario`).
- **Download games from Flashpoint**: in the Import tab, **X** searches the Flashpoint Archive and shows the results as a cover grid; **A** downloads a game's `.swf` directly. Its cover is fetched automatically, and its real title is kept even when the filename cannot hold characters like `:`. Press **+** on a result to see its full title, developer, publisher, release date and download size.
- **Import as a list**: the Import tab is a list of your saved URLs. Press **A** to launch one, use the **+ Add a URL** row to enter a new one, and **+** on a URL to edit or delete it. It accepts archive.org items and direct `.swf` URLs.
- **Sort your library** (**Y** in the Play tab): by name, date added, last played, most played, or size. **X** reverses the order, and the choice is saved.
- **Playtime**: each game tracks how long you have played it (shown under the selected game, and used by the "most played" sort).
- **Report a bug or send a suggestion** (Settings tab): flag a game that renders or plays wrong, or send a feature idea. It opens an issue on the FlashNX repository, with no account and no login.

### Changes

- **Controls**: **−** is search, **+** is the selected game's options; default controls, language, bug report, suggestion and **Quit** all live in the Settings tab. Switching tabs is **L / R** only, and **B** always just backs out of a modal (the redundant "Back" rows were removed).
- **Audio level**: the in-app sound now matches the rest of the Switch (it used to be noticeably louder).

### Fixes

- **Large backgrounds no longer turn white**: games whose backdrop or floor is a bitmap wider or taller than 2048px (for example Mario Combat's sky and ground) used to render as solid white blocks. They now draw correctly.
- **Deleting a game cleans up everything**: removing a game now also deletes its cached online cover and the cover sidecars saved under the plain game name (on top of the `.swf` and its keymap/rename/save files), and clears the leftover Import-list "downloaded" badge and the on-screen cover, so re-importing the same game later starts fresh.
- **Flashpoint cover grids no longer freeze the UI**: logos load in the background, so a broad search with dozens of results stays responsive while the thumbnails fill in.
- **Missing accents** restored on several labels (the sort options, "edit", "download").

### Notes

- Covers and downloads use the public Flashpoint Archive APIs (metadata, logos, GameZIP). Downloading a game is always something you choose, one game at a time.
- Bug reports and suggestions are anonymous: they go through a small relay that opens a GitHub issue, so you never need an account or to log in.

## v1.1.1 (2026-06-05)

Data-safety and import-diagnostics fixes, plus library search.

### Fixes

- **URL history no longer disappears in applet mode**: history (and saves, settings, renamed-game sidecars) is now read with a bounded reader and committed to the SD card after every write, so it survives switching between applet (album takeover) and full title-takeover modes. Previously the history could read empty in applet mode, or get overwritten by the next change.
- **HTTPS import errors are now readable**: a failed import shows the real cause (libcurl code and message, HTTP status) instead of an opaque "code -2", so you can tell whether it is the console clock, DNS, the certificate bundle, or a blocked link.

### Features

- **Search the local library**: press **X** to filter the game list by name (empty input clears the filter), the same way the archive.org screen already works.
- **Clear applet-mode notice**: trying to launch a game without the full app memory now shows a readable message ("launch via title takeover") instead of a red screen.

### Changes

- **More consistent controls**: **X** is search on every list, and **ZL** manages the selected item (game options in the library, delete URL in the import history). The on-screen footers reflect the new layout.
- The default example URL is now a neutral placeholder.

## v1.1.0 — 2026-06-04

Localization update + UI polish.

### Features

- **Multi-language UI**: the whole interface is now available in **English, French, Spanish and Russian**. The language is auto-detected from the console's system language on first boot, and can be changed at any time.
- **Settings modal (`+` in the library)**: a new global settings screen with two entries — **default controls** (edit the global default keymap used by every game without a per-game override) and **language**.
- **URL history management**: in the archive.org import screen, **X** removes the currently-shown URL from the history (with a confirmation modal).
- **Quit returns to the right row**: leaving a game (pause menu → QUIT) lands the cursor back on the game you were playing, instead of jumping to the top of the list.
- **Pause menu shows the game's name** under "PAUSE" (like the OPTIONS modal).
- **Library shows 8 games** at once (was 6).
- Pixel font extended with **uppercase Cyrillic** (Russian), **French/Spanish accents** (É È À Ç / Á Í Ó Ú Ñ ¿ ¡), the **apostrophe** (`'`), and previously-missing punctuation (`,` `?` `+` `(` `)` `[` `]` `<` `%` `…`), which also improves the existing locales.

### Notes

- The chosen language is persisted to `sdmc:/flashnx/settings.json`.
- Flash key names (`Space`, `Shift`, `A`…`Z`) are technical identifiers and remain untranslated; only UI labels and messages are localized.
- Opening the settings modal returns to the previously-selected game row (like the OPTIONS modal).

## v1.0.0 — 2026-05-31

First official release. FlashNX runs AS1/AS2 Flash games (and part of AS3) straight from your Switch's SD card.

### Features

- **Full Flash player**: Ruffle core (SWF parsing + AVM1/AVM2 interpreter) wired onto a native Switch stack — OpenGL rendering (switch-mesa), audio (audren), joycon + mouse input (right stick / touchscreen).
- **FlashNX library**: joycon-navigable interface, banner + per-game thumbnail, `AS3` badge for AVM2 games, game renaming (without touching the file), automatic `.swf` detection on SD.
- **archive.org remote import**: download `.swf` files over HTTPS directly from the Switch (software keyboard, URL history, progress bar).
- **In-game key editor**: remap the 48 supported Flash keys per game, from the pause menu or the library.
- **Native saves**: games that save via `SharedObject` (`.sol`) keep your progress on the SD card.
- **Robustness**: anti-fragmentation GL mega-arena, handling of bitmaps > 2048 px, native exception handler with a symbolizable crash log. The `.nro` never refuses to boot (built-in fallback).

### Games tested on hardware

Super Mario 63, Super Mario World Flash, Mario Forever Flash, Tetris'd, Flappy Bird, There Is Only One Level, Mario 3D Racing, Pursuit of Hat 2, and others. Most run at 55-60 fps.

### Known limitations (acknowledged)

- **Heavy-game performance**: on some titles (Mario 63 in dense scenes, complex AS3 games like Pursuit of Hat 2), frame-rate drops come from **Ruffle's AVM2/AVM1 interpreter** (CPU-bound, no JIT), not from rendering — not fixable from the backend. Measured: our rendering stays around ~5 ms/frame while the game logic can take >1 s on a single frame. Out-of-app lever: CPU overclock (sys-clk).
- **Partial AS3/AVM2 compatibility**: inherited from the upstream Ruffle engine (see [ruffle.rs/compatibility](https://ruffle.rs/compatibility)). Games showing an `AS3` badge in the library have variable support.
- **No savestate / rewind**: Ruffle does not expose a snapshot of the execution state (the state is a `gc-arena` object graph, not trivially serializable). Games' native saves (`.sol`) do work.
- **Audio**: the sound is now soft-limited (loud, without hard clipping). On **very** dense scenes (Mario 63), a slight occasional crackle may remain (peak compression) — to be refined in an update.

### Installation

1. Copy `FlashNX.nro` into `sdmc:/switch/` (or `sdmc:/switch/FlashNX/`).
2. Copy your `.swf` files into `sdmc:/flashnx/`.
3. Launch FlashNX from the Homebrew Menu.

### Credits

- **Author**: Jonathan8520
- **Powered by Ruffle** (Apache-2.0 / MIT) — the Flash emulation core.
- Native Switch stack via devkitPro / libnx / switch-mesa.
