package dev.constnishant.sonorus.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.LibraryMusic
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.player.MiniPlayer
import dev.constnishant.sonorus.ui.player.NowPlayingScreen
import dev.constnishant.sonorus.ui.player.showsPause
import dev.constnishant.sonorus.ui.screens.AlbumScreen
import dev.constnishant.sonorus.ui.screens.ArtistScreen
import dev.constnishant.sonorus.ui.screens.HomeScreen
import dev.constnishant.sonorus.ui.screens.LibraryScreen
import dev.constnishant.sonorus.ui.screens.PlaylistScreen
import dev.constnishant.sonorus.ui.screens.SearchScreen
import dev.constnishant.sonorus.ui.settings.SettingsScreen
import dev.constnishant.sonorus.ui.uploads.UploadsScreen

private data class TopLevelTab(val route: String, @StringRes val labelRes: Int, val icon: ImageVector)

private val TopLevelTabs = listOf(
    TopLevelTab(Routes.HOME, R.string.nav_home, Icons.Rounded.Home),
    TopLevelTab(Routes.LIBRARY, R.string.nav_library, Icons.Rounded.LibraryMusic),
    TopLevelTab(Routes.SEARCH, R.string.nav_search, Icons.Rounded.Search),
)

/** Navigation arguments for the detail routes: a single non-null library id. */
private val IdArgs = listOf(navArgument(Routes.ARG_ID) { type = NavType.LongType })

private fun NavBackStackEntry.idArg(): Long =
    checkNotNull(arguments?.getLong(Routes.ARG_ID)) { "missing ${Routes.ARG_ID}" }

/**
 * The app shell: bottom navigation, mini player, snackbar host and the navigation graph. The
 * bottom bar and the mini player are hidden on the Now Playing and Settings screens.
 */
@Composable
fun SonorusRoot() {
    val repository = LocalContext.current.repository
    val navController = rememberNavController()
    val playback by repository.events.playback.collectAsStateWithLifecycle()
    val backStackEntry by navController.currentBackStackEntryAsState()
    val destination = backStackEntry?.destination
    val showChrome = destination?.route != Routes.NOW_PLAYING && destination?.route != Routes.SETTINGS
    val snackbarHostState = remember { SnackbarHostState() }
    // The tab the user is in. Detail screens are not nested under a tab in the graph, so the tab
    // is remembered from the last top-level destination instead of derived from the current one.
    var currentTab by rememberSaveable { mutableStateOf(Routes.HOME) }
    LaunchedEffect(destination?.route) {
        val route = destination?.route
        if (TopLevelTabs.any { it.route == route } && route != null) currentTab = route
    }

    LaunchedEffect(repository) {
        repository.events.errors.collect { snackbarHostState.showSnackbar(it) }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        bottomBar = {
            if (showChrome) {
                Column {
                    if (playback.track != null) {
                        MiniPlayer(
                            playback = playback,
                            onOpen = { navController.navigate(Routes.NOW_PLAYING) },
                            onPlayPause = {
                                val pause = playback.state.showsPause()
                                repository.command { if (pause) it.pause() else it.play() }
                            },
                            onNext = { repository.command { it.next() } },
                        )
                    }
                    NavigationBar {
                        TopLevelTabs.forEach { tab ->
                            NavigationBarItem(
                                selected = currentTab == tab.route,
                                onClick = {
                                    if (currentTab == tab.route) {
                                        // Re-selecting the current tab returns to its top screen.
                                        navController.popBackStack(tab.route, inclusive = false)
                                    } else {
                                        navController.navigate(tab.route) {
                                            popUpTo(Routes.HOME) { saveState = true }
                                            launchSingleTop = true
                                            restoreState = true
                                        }
                                    }
                                },
                                icon = { Icon(tab.icon, contentDescription = null) },
                                label = { Text(stringResource(tab.labelRes)) },
                            )
                        }
                    }
                }
            }
        },
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = Routes.HOME,
            modifier = Modifier.padding(innerPadding),
        ) {
            composable(Routes.HOME) {
                HomeScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onOpenSettings = { navController.navigate(Routes.SETTINGS) },
                )
            }
            composable(Routes.LIBRARY) {
                LibraryScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                    onPlaylist = { navController.navigate(Routes.playlist(it)) },
                )
            }
            composable(Routes.SEARCH) {
                SearchScreen(
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                )
            }
            composable(Routes.ALBUM, arguments = IdArgs) { entry ->
                AlbumScreen(
                    albumId = entry.idArg(),
                    onArtist = { navController.navigate(Routes.artist(it)) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.ARTIST, arguments = IdArgs) { entry ->
                ArtistScreen(
                    artistId = entry.idArg(),
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.PLAYLIST, arguments = IdArgs) { entry ->
                PlaylistScreen(
                    playlistId = entry.idArg(),
                    onBack = { navController.popBackStack() },
                )
            }
            composable(Routes.NOW_PLAYING) {
                NowPlayingScreen(
                    onBack = { navController.popBackStack() },
                    onAlbum = { navController.navigate(Routes.album(it)) },
                    onArtist = { navController.navigate(Routes.artist(it)) },
                )
            }
            composable(Routes.SETTINGS) {
                SettingsScreen(
                    onBack = { navController.popBackStack() },
                    onUploads = { navController.navigate(Routes.UPLOADS) },
                )
            }
            composable(Routes.UPLOADS) {
                UploadsScreen(onBack = { navController.popBackStack() })
            }
        }
    }
}
