# Цели Mixin'ов в модах

`cargo xtask mixins` (2026-10-26): 500 самых скачиваемых модов с открытой лицензией (SPDX, не
`LicenseRef-…`) на Modrinth, у которых есть сборка под 26.x — с 26.1 игра не обфусцирована,
так что цели Mixin'ов — имена Mojang. Из jar'ов (и вложенных `META-INF/jars`) читаются
только аннотации `@Mixin` и инжекторов (`@Inject`, `@Redirect`, `@Modify*`, `@Overwrite`,
MixinExtras); `@Accessor`/`@Invoker` не считаются патчем. Счёт — число проектов Modrinth на
цель; вложенная библиотека засчитывается одному проекту. Полные таблицы:
`cache/mixins/methods.tsv`, `classes.tsv`.

**Итог:** 2109 ванильных классов и 5068 методов патчатся хотя бы одним модом; из них вне
`client` — 1447 классов и 3214 методов. Сторона (`common`/`client`/`both`) — из секции
mixin-конфига, где мод объявил миксин.

Инжекторы по числу (мод × цель × вид): Inject 5506, WrapOperation 857, Redirect 593, ModifyExpressionValue 435, ModifyVariable 306, ModifyArg 287, WrapMethod 281, Overwrite 280, ModifyReturnValue 277, ModifyConstant 216, WrapWithCondition 170, ModifyArgs 24, Definition 15, ModifyReceiver 11.

## Вывод для API модов (Ф8)

- Клиент патчат больше всего (`Minecraft`, ввод, HUD, рендер) — это вход для клиентских
  модулей Ф11, не для серверного API.
- На сервере моды цепляются за жизненный цикл (старт/стоп сервера, вход игрока, смерть,
  урон), тик сущностей и мира, предметы и чанки. Эти точки — первые кандидаты в хуки:
  события «до/после» с возможностью отменить и поменять аргумент покрывают большинство
  (`Inject` — около 59% всех патчей).
- Дальше — `ModifyVariable`/`ModifyArg`/`ModifyExpressionValue`: моды правят числа внутри
  методов (урон, скорость, дальность). Для них нужны хуки на значения, а не только события.

## Серверные методы — топ-60

| # | метод | модов | сторона | виды |
|---|---|---|---|---|
| 1 | `server.players.PlayerList.placeNewPlayer` | 17 | both | Inject 16, Redirect 1, WrapOperation 1 |
| 2 | `world.entity.LivingEntity.hurtServer` | 16 | common | Inject 10, ModifyArg 2, ModifyVariable 6, Redirect 1, WrapMethod 1, WrapOperation 1 |
| 3 | `server.MinecraftServer.stopServer` | 14 | both | Inject 13, WrapWithCondition 1 |
| 4 | `world.entity.player.Player.tick` | 13 | both | Inject 12, Redirect 1, WrapOperation 1 |
| 5 | `server.MinecraftServer.runServer` | 12 | both | Inject 11, Redirect 1, WrapMethod 1 |
| 6 | `server.level.ServerLevel.<init>` | 12 | common | Inject 11, Redirect 1, WrapOperation 1 |
| 7 | `world.entity.LivingEntity.tickEffects` | 12 | both | Inject 5, ModifyExpressionValue 2, Redirect 1, WrapOperation 2, WrapWithCondition 2 |
| 8 | `server.level.ServerLevel.tick` | 10 | common | Inject 10 |
| 9 | `server.level.ServerPlayer.die` | 10 | common | Inject 10 |
| 10 | `world.entity.LivingEntity.die` | 10 | common | Inject 10, Redirect 1, WrapOperation 1 |
| 11 | `world.entity.LivingEntity.tick` | 10 | both | Inject 10 |
| 12 | `world.item.ItemStack.addDetailsToTooltip` | 10 | both | Inject 8, ModifyArg 2, ModifyExpressionValue 2, WrapWithCondition 1 |
| 13 | `world.level.chunk.LevelChunk.<init>` | 10 | both | Inject 10, ModifyVariable 1, Redirect 1 |
| 14 | `server.Main.main` | 9 | common | Inject 8, WrapWithCondition 1 |
| 15 | `server.level.ServerPlayer.restoreFrom` | 9 | common | Inject 9 |
| 16 | `world.entity.Entity.load` | 9 | common | Inject 9 |
| 17 | `commands.Commands.<init>` | 8 | common | Inject 8 |
| 18 | `server.PlayerAdvancements.award` | 8 | common | Inject 8 |
| 19 | `server.level.ServerLevel.tickChunk` | 8 | common | Inject 5, ModifyArg 1, ModifyExpressionValue 1, Redirect 1, WrapMethod 1, WrapOperation 1 |
| 20 | `world.entity.Entity.saveWithoutId` | 8 | common | Inject 8 |
| 21 | `world.entity.LivingEntity.aiStep` | 8 | both | Inject 3, ModifyArg 3, ModifyConstant 1, ModifyExpressionValue 2, Redirect 1, WrapMethod 1, WrapOperation 1 |
| 22 | `world.entity.LivingEntity.createLivingAttributes` | 8 | common | Inject 4, ModifyExpressionValue 1, ModifyReturnValue 2, WrapMethod 1 |
| 23 | `world.entity.player.Player.attack` | 8 | common | Inject 7, ModifyArg 1, ModifyVariable 1, WrapOperation 1 |
| 24 | `world.entity.player.Player.getDestroySpeed` | 8 | both | Inject 5, ModifyExpressionValue 2, ModifyReturnValue 1, Redirect 1, WrapOperation 2 |
| 25 | `world.inventory.AnvilMenu.createResult` | 8 | common | Inject 6, ModifyExpressionValue 1, Redirect 1 |
| 26 | `world.level.chunk.storage.SerializableChunkData.read` | 8 | common | Inject 6, ModifyVariable 1, WrapOperation 1 |
| 27 | `server.MinecraftServer.tickServer` | 7 | common | Inject 7 |
| 28 | `server.ReloadableServerResources.updateComponentsAndStaticRegistryTags` | 7 | common | Inject 7 |
| 29 | `server.network.ServerGamePacketListenerImpl.handleUseItemOn` | 7 | common | Inject 3, WrapOperation 5 |
| 30 | `util.datafix.fixes.ItemStackComponentizationFix.fixItemStack` | 7 | common | Inject 7 |
| 31 | `world.entity.Entity.<init>` | 7 | both | Inject 6, ModifyArg 2 |
| 32 | `world.entity.LivingEntity.baseTick` | 7 | both | Inject 4, ModifyConstant 1, ModifyExpressionValue 1, WrapWithCondition 1 |
| 33 | `world.entity.LivingEntity.dropExperience` | 7 | common | Inject 2, ModifyArg 2, Redirect 1, WrapOperation 2 |
| 34 | `world.entity.LivingEntity.jumpFromGround` | 7 | both | Inject 6, Redirect 1 |
| 35 | `world.entity.item.FallingBlockEntity.tick` | 7 | both | Inject 6, WrapOperation 1 |
| 36 | `world.entity.item.ItemEntity.playerTouch` | 7 | common | Inject 7 |
| 37 | `world.entity.projectile.arrow.AbstractArrow.onHitEntity` | 7 | both | Inject 4, ModifyVariable 1, WrapOperation 2, WrapWithCondition 1 |
| 38 | `world.level.chunk.ChunkGenerator.tryGenerateStructure` | 7 | common | Inject 7 |
| 39 | `server.level.ServerPlayer.tick` | 6 | common | Inject 5, WrapWithCondition 1 |
| 40 | `server.level.ServerPlayerGameMode.destroyBlock` | 6 | common | Inject 6 |
| 41 | `server.packs.repository.PackRepository.<init>` | 6 | both | Inject 6 |
| 42 | `server.packs.resources.ReloadableResourceManager.createReload` | 6 | both | Inject 4, ModifyArg 2 |
| 43 | `server.players.PlayerList.respawn` | 6 | common | Inject 6, Redirect 1 |
| 44 | `world.entity.Entity.startRiding` | 6 | both | Inject 2, ModifyExpressionValue 2, ModifyReturnValue 1, Redirect 1 |
| 45 | `world.entity.Entity.tick` | 6 | both | Inject 6 |
| 46 | `world.entity.ExperienceOrb.playerTouch` | 6 | common | Inject 5, WrapMethod 1 |
| 47 | `world.entity.LivingEntity.addEffect` | 6 | common | Inject 4, ModifyVariable 1, WrapMethod 1 |
| 48 | `world.entity.LivingEntity.travelInAir` | 6 | both | ModifyConstant 1, ModifyExpressionValue 1, Redirect 1, WrapOperation 3 |
| 49 | `world.entity.player.Player.actuallyHurt` | 6 | common | Inject 2, ModifyReceiver 1, ModifyVariable 1, WrapOperation 2 |
| 50 | `world.entity.player.Player.interactOn` | 6 | both | Inject 6 |
| 51 | `world.entity.player.Player.readAdditionalSaveData` | 6 | common | Inject 6 |
| 52 | `world.item.BlockItem.place` | 6 | both | Inject 3, WrapOperation 2, WrapWithCondition 1 |
| 53 | `world.item.ItemStack.addToTooltip` | 6 | both | Inject 5, ModifyExpressionValue 1 |
| 54 | `world.item.ItemStack.getTooltipLines` | 6 | both | Inject 6 |
| 55 | `world.level.BaseSpawner.serverTick` | 6 | common | Inject 3, ModifyArg 3, ModifyVariable 1, Redirect 1, WrapOperation 1 |
| 56 | `world.level.Level.<init>` | 6 | both | Inject 4, Redirect 1, WrapOperation 1 |
| 57 | `world.level.ServerExplosion.hurtEntities` | 6 | both | Inject 2, ModifyVariable 2, Redirect 1, WrapOperation 1 |
| 58 | `world.level.chunk.storage.SerializableChunkData.copyOf` | 6 | common | Inject 5, WrapOperation 2 |
| 59 | `world.level.entity.PersistentEntitySectionManager.addEntity` | 6 | common | Inject 6 |
| 60 | `world.level.levelgen.structure.templatesystem.StructureTemplate.placeInWorld` | 6 | common | Inject 4, Redirect 2 |

## Серверные классы — топ-40

| # | класс | модов | сторона |
|---|---|---|---|
| 1 | `world.entity.Entity` | 76 | both |
| 2 | `world.entity.LivingEntity` | 75 | both |
| 3 | `world.entity.player.Player` | 55 | both |
| 4 | `server.level.ServerLevel` | 45 | common |
| 5 | `world.item.ItemStack` | 42 | both |
| 6 | `server.level.ServerPlayer` | 40 | common |
| 7 | `server.MinecraftServer` | 39 | both |
| 8 | `server.network.ServerGamePacketListenerImpl` | 31 | common |
| 9 | `server.players.PlayerList` | 29 | both |
| 10 | `world.entity.Mob` | 29 | both |
| 11 | `world.level.Level` | 25 | both |
| 12 | `world.level.block.Block` | 25 | both |
| 13 | `server.level.ChunkMap` | 23 | common |
| 14 | `world.item.Item` | 21 | both |
| 15 | `world.inventory.AbstractContainerMenu` | 20 | both |
| 16 | `world.level.block.state.BlockBehaviour` | 19 | both |
| 17 | `world.level.block.state.BlockBehaviour$BlockStateBase` | 19 | both |
| 18 | `network.Connection` | 17 | both |
| 19 | `world.level.block.entity.BlockEntity` | 17 | both |
| 20 | `world.level.chunk.LevelChunk` | 17 | both |
| 21 | `server.level.ServerPlayerGameMode` | 16 | common |
| 22 | `commands.Commands` | 15 | common |
| 23 | `world.entity.item.ItemEntity` | 15 | both |
| 24 | `world.level.chunk.ChunkGenerator` | 14 | common |
| 25 | `world.item.crafting.RecipeManager` | 13 | common |
| 26 | `world.item.BlockItem` | 12 | both |
| 27 | `world.level.NaturalSpawner` | 12 | common |
| 28 | `world.level.biome.Biome` | 12 | both |
| 29 | `server.ReloadableServerResources` | 11 | common |
| 30 | `server.network.ServerCommonPacketListenerImpl` | 11 | common |
| 31 | `server.packs.repository.PackRepository` | 11 | both |
| 32 | `world.entity.animal.equine.AbstractHorse` | 11 | both |
| 33 | `world.entity.player.Inventory` | 11 | both |
| 34 | `world.entity.projectile.FishingHook` | 11 | both |
| 35 | `world.level.levelgen.structure.templatesystem.StructureTemplate` | 11 | common |
| 36 | `core.MappedRegistry` | 10 | both |
| 37 | `server.level.ServerChunkCache` | 10 | common |
| 38 | `world.entity.ExperienceOrb` | 10 | common |
| 39 | `world.entity.vehicle.boat.AbstractBoat` | 10 | both |
| 40 | `world.inventory.AnvilMenu` | 10 | both |

## Все классы — топ-20 (с клиентом)

| # | класс | модов | сторона |
|---|---|---|---|
| 1 | `client.Minecraft` | 126 | both |
| 2 | `world.entity.Entity` | 76 | both |
| 3 | `world.entity.LivingEntity` | 75 | both |
| 4 | `client.multiplayer.ClientPacketListener` | 65 | both |
| 5 | `client.gui.Hud` | 59 | both |
| 6 | `world.entity.player.Player` | 55 | both |
| 7 | `client.renderer.GameRenderer` | 51 | both |
| 8 | `client.gui.screens.Screen` | 50 | both |
| 9 | `client.gui.GuiGraphicsExtractor` | 49 | both |
| 10 | `client.multiplayer.ClientLevel` | 48 | both |
| 11 | `client.gui.Gui` | 47 | both |
| 12 | `client.player.LocalPlayer` | 45 | client |
| 13 | `server.level.ServerLevel` | 45 | common |
| 14 | `client.MouseHandler` | 42 | both |
| 15 | `world.item.ItemStack` | 42 | both |
| 16 | `server.level.ServerPlayer` | 40 | common |
| 17 | `client.gui.screens.inventory.AbstractContainerScreen` | 39 | both |
| 18 | `client.renderer.LevelRenderer` | 39 | both |
| 19 | `server.MinecraftServer` | 39 | both |
| 20 | `client.KeyboardHandler` | 38 | both |
