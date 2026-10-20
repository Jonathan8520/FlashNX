package com.clubpenguin
{
   import com.clubpenguin.game.ICPHost;
   import com.clubpenguin.lib.vo.IEndGameVO;
   import com.clubpenguin.lib.vo.IPlayerVO;
   import com.disney.coupler.ICouplerGame;
   import com.disney.coupler.ICouplerHost;
   import flash.display.Loader;
   import flash.display.LoaderInfo;
   import flash.display.Sprite;
   import flash.events.Event;
   import flash.net.URLRequest;

   // FlashNX: Club Penguin's side of a minigame, as far as Pufflescape needs
   // it. The game asks its host which stamps the player has to open its
   // levels (all of them here) and tells it when a level is done.
   public class CPHostEmulator extends Sprite implements ICouplerHost, ICPHost
   {
      protected var __hostedGameName:String;

      protected var __hostedGame:ICouplerGame;

      public function CPHostEmulator()
      {
         super();
         this.__hostedGameName = this.getHostGameName();
         var loader:Loader = new Loader();
         loader.contentLoaderInfo.addEventListener(Event.COMPLETE, this.onLoadComplete);
         loader.load(new URLRequest(this.__hostedGameName));
      }

      protected function getHostGameName() : String
      {
         return "";
      }

      private function onLoadComplete(e:Event) : void
      {
         this.__hostedGame = (e.target as LoaderInfo).content as ICouplerGame;
         this.__hostedGame.setHost(this);
      }

      public function launchGame() : void
      {
      }

      public function getBasePath() : String
      {
         return "";
      }

      public function isMusicMuted() : Boolean
      {
         return false;
      }

      public function getPlayerVO() : IPlayerVO
      {
         return null;
      }

      public function getLocale() : int
      {
         return -1;
      }

      public function levelComplete(level:int) : void
      {
      }

      public function levelStart(level:int) : void
      {
      }

      public function stampEarned(stamp:int, flag:Boolean = false) : void
      {
      }

      public function isStampEarned(stamp:int) : Boolean
      {
         return true;
      }

      public function requestFPS(fps:int) : Boolean
      {
         return false;
      }

      public function showEndGame(vo:IEndGameVO) : void
      {
      }

      public function showMembershipPrompt(a:String, b:String = null) : void
      {
      }

      public function getMyPuffles() : Array
      {
         return null;
      }
   }
}
