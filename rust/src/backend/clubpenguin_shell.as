// FlashNX: Club Penguin's shell and game server, as far as a minigame's save
// needs them (#134). Compiled into clubpenguin_shell.bin (the AVM1 bytecode of
// one DoAction) and put at the start of the game's first frame by
// clubpenguin.rs. To rebuild it: a one-frame SWF 9 AS2 movie with an empty
// DoAction, then JPEXS FFDec:
//   ffdec-cli -replace template.swf shim.swf "\frame_1\DoAction" clubpenguin_shell.as
// and the body of shim.swf's DoAction tag is the .bin.
//
// SaveData (com.clubpenguin.lib.data) sends "sgd" with the compressed save,
// after its slot number when it has one (DJ3000 keeps six mixes), and "ggd",
// with the slot or without, to have it back, through
// _global.getCurrentAirtower(). Answered as Houdini, the Club Penguin server
// emulator, does (solero/houdini, handlers/games): "ggd" gets the slot's data,
// or "" when there is none (an empty data set, a new player), the slot being 0
// when none is given; "sgd" gets no answer. The answer reaches the listeners
// registered for its message with [room, data], read as responseArray[1].
// DJ3000 also has the server approve a mix's name ("vn"), and takes its own
// name coming back as the approval. Here the saves are a SharedObject, so a
// .sol on the card like any other game's, and the answers come a moment
// later, as from a server.
//
// Listeners as the real client keeps them (com.clubpenguin.net.Airtower, in
// the CPImagined archive): one per function, whatever the object. Each
// SaveData removes its function and adds it again, so the last one made gets
// the answers; DJ3000 makes two, and two listeners would load every mix twice.
_global.flashnxClubPenguinInit = function()
{
   if (_global.getCurrentShell != undefined)
   {
      return;
   }
   var cp = new Object();
   cp.listeners = new Object();
   cp.shell = new Object();
   // A member: both games keep their saves for members only. Except for
   // the one test Flashpoint turned around in its DJ3000 (the saved mixes
   // open for a non-member there, as it had no client to ask).
   cp.shell.isMyPlayerMember = function()
   {
      var chooser = _global.com.clubpenguin.games.beatbox.SongChooser;
      if (chooser != undefined && arguments.caller == chooser.prototype.showSavedMixes)
      {
         return false;
      }
      return true;
   };
   cp.shell.getCurrentServerRoomId = function()
   {
      return -1;
   };
   cp.airtower = new Object();
   cp.airtower.addListener = function(type, func, scope)
   {
      var l = _global.flashnxClubPenguin.listeners;
      if (l[type] == undefined)
      {
         l[type] = new Array();
      }
      var a = l[type];
      var i = 0;
      while (i < a.length)
      {
         if (a[i].f == func)
         {
            return false;
         }
         i++;
      }
      a.push({f:func, s:scope});
      return true;
   };
   cp.airtower.removeListener = function(type, func)
   {
      var a = _global.flashnxClubPenguin.listeners[type];
      var i = 0;
      while (i < a.length)
      {
         if (a[i].f == func)
         {
            a.splice(i, 1);
            return true;
         }
         i++;
      }
      return false;
   };
   cp.airtower.send = function(ext, cmd, args, kind, room)
   {
      var so = SharedObject.getLocal("flashnx_clubpenguin");
      var reply;
      var slot = 0;
      if (cmd == "sgd")
      {
         var data = args[args.length - 1];
         if (args.length >= 2)
         {
            slot = args[0];
         }
         so.data["slot" + slot] = String(data);
         so.flush();
         return;
      }
      else if (cmd == "ggd")
      {
         if (typeof(args) == "object" && args.length >= 1)
         {
            slot = args[0];
         }
         var saved = so.data["slot" + slot];
         reply = [room, saved == undefined ? "" : saved];
      }
      else if (cmd == "vn")
      {
         reply = [room, args[0]];
      }
      else
      {
         return;
      }
      var id;
      id = setInterval(function()
      {
         clearInterval(id);
         var all = _global.flashnxClubPenguin.listeners[cmd];
         var a = all.slice();
         var i = 0;
         while (i < a.length)
         {
            if (a[i].s)
            {
               a[i].f.call(a[i].s, reply);
            }
            else
            {
               a[i].f(reply);
            }
            i++;
         }
      }, 1);
   };
   _global.flashnxClubPenguin = cp;
   _global.getCurrentShell = function()
   {
      return _global.flashnxClubPenguin.shell;
   };
   _global.getCurrentAirtower = function()
   {
      return _global.flashnxClubPenguin.airtower;
   };
};
_global.flashnxClubPenguinInit();
delete _global.flashnxClubPenguinInit;
