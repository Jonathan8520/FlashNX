// FlashNX: Club Penguin's shell and game server, as far as a minigame's save
// needs them (#134). Compiled into clubpenguin_shell.bin (the AVM1 bytecode of
// one DoAction) and put at the start of the game's first frame by
// clubpenguin.rs. To rebuild it: a one-frame SWF 9 AS2 movie with an empty
// DoAction, then JPEXS FFDec:
//   ffdec-cli -replace template.swf shim.swf "\frame_1\DoAction" clubpenguin_shell.as
// and the body of shim.swf's DoAction tag is the .bin.
//
// SaveData (com.clubpenguin.lib.data) sends "sgd" with the compressed save and
// "ggd" to have it back, through _global.getCurrentAirtower(); the answer to
// "ggd" reaches the listener registered for "ggd", which reads the save in
// responseArray[1] ("" when there is none: an empty data set, a new player).
// Here the save is a SharedObject, so a .sol on the card like any other game's,
// and the answer comes a moment later, as from a server.
_global.flashnxClubPenguinInit = function()
{
   if (_global.getCurrentShell != undefined)
   {
      return;
   }
   var cp = new Object();
   cp.listeners = new Object();
   cp.shell = new Object();
   cp.shell.isMyPlayerMember = function()
   {
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
      l[type].push({f:func, s:scope});
   };
   cp.airtower.removeListener = function(type, func, scope)
   {
      var a = _global.flashnxClubPenguin.listeners[type];
      var i = a.length - 1;
      while (i >= 0)
      {
         if (a[i].f == func && a[i].s == scope)
         {
            a.splice(i, 1);
         }
         i--;
      }
   };
   cp.airtower.send = function(ext, cmd, args, kind, room)
   {
      var so = SharedObject.getLocal("flashnx_clubpenguin");
      var reply;
      if (cmd == "sgd")
      {
         so.data.gameData = String(args[0]);
         so.flush();
         reply = [room];
      }
      else if (cmd == "ggd")
      {
         reply = [room, so.data.gameData == undefined ? "" : so.data.gameData];
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
            a[i].f.apply(a[i].s, [reply]);
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
