package
{
   import com.clubpenguin.CPHostEmulator;
   import flash.display.DisplayObject;
   import flash.events.Event;
   import flash.net.SharedObject;
   import flash.system.ApplicationDomain;

   // FlashNX: Pufflescape's host, which also keeps the game's scores.
   // The game counts each level's O-berries, its rare berry and its time on
   // the level select's icons, and forgot them when it closed (Club Penguin
   // only kept the stamps). Twice a second this reads the icons' scores and
   // writes the completed ones to a SharedObject (a .sol on the SD card);
   // once the level select exists, the kept scores go back on its icons
   // through the game's own LevelIcon.updateScore, as after playing them.
   [SWF(width="760", height="480", backgroundColor="#869ca7", frameRate="45")]
   public class HostEmulator extends CPHostEmulator
   {
      private static const SELECT:String = "com.clubpenguin.games.collisioncourse.ui.LevelSelectUI";

      private static const SCORE:String = "com.clubpenguin.games.collisioncourse.levels.Score";

      private var __save:SharedObject;

      private var __restored:Boolean = false;

      private var __written:String = "";

      private var __frames:int = 0;

      public function HostEmulator()
      {
         super();
         try
         {
            this.__save = SharedObject.getLocal("flashnx_pufflescape");
         }
         catch(e:Error)
         {
            this.__save = null;
         }
         addEventListener(Event.ENTER_FRAME, this.keepScores);
      }

      override protected function getHostGameName() : String
      {
         return "RollerscapeCP_CP.swf";
      }

      private function keepScores(e:Event) : void
      {
         this.__frames++;
         if(this.__frames % 20 != 0 || this.__save == null || this.__hostedGame == null)
         {
            return;
         }
         var domain:ApplicationDomain = DisplayObject(this.__hostedGame).loaderInfo.applicationDomain;
         if(!domain.hasDefinition(SELECT) || !domain.hasDefinition(SCORE))
         {
            return;
         }
         var select:Object = domain.getDefinition(SELECT);
         var ui:Object = select.instance;
         if(ui == null || ui.levelIcons == null)
         {
            return;
         }
         var icons:Object = ui.levelIcons;
         var names:Array = [];
         var name:String;
         for(name in icons)
         {
            names.push(name);
         }
         if(names.length == 0)
         {
            return;
         }
         names.sort();
         if(!this.__restored)
         {
            this.__restored = true;
            this.restore(icons, domain.getDefinition(SCORE) as Class);
         }
         var scores:Object = {};
         var text:String = "";
         for each(name in names)
         {
            var s:Object = icons[name].currentScore;
            if(s == null)
            {
               continue;
            }
            if(s.normalCollectables > 0 || s.hardCollectable || s.timeToComplete != null)
            {
               var time:Number = s.timeToComplete != null ? Number(s.timeToComplete.getTime()) : 0;
               scores[name] = [Number(s.normalCollectables), Boolean(s.hardCollectable), time];
               text += name + ":" + s.normalCollectables + "," + s.hardCollectable + "," + time + ";";
            }
         }
         if(text != this.__written)
         {
            this.__written = text;
            this.__save.data.scores = scores;
            try
            {
               this.__save.flush();
            }
            catch(e:Error)
            {
            }
         }
      }

      private function restore(icons:Object, scoreClass:Class) : void
      {
         var kept:Object = this.__save.data.scores;
         if(kept == null || scoreClass == null)
         {
            return;
         }
         var name:String;
         for(name in kept)
         {
            var icon:Object = icons[name];
            var k:Array = kept[name] as Array;
            if(icon == null || k == null || k.length < 3)
            {
               continue;
            }
            var time:Date = Number(k[2]) > 0 ? new Date(Number(k[2])) : null;
            icon.updateScore(new scoreClass(Number(k[0]), icon.currentScore.totalNormalCollectables, Boolean(k[1]), time));
         }
      }
   }
}
