![](https://stumpwm.github.io/images/stumpwm-logo-stripe.png)
# The Stump Window Manager
![](https://travis-ci.org/stumpwm/stumpwm.svg)

Stumpwm is a window manager written entirely in Common Lisp. It
attempts to be highly customizable while relying entirely on the
keyboard for input. You will not find buttons, icons, title bars, tool
bars, or any of the other conventional GUI widgets.

These design decisions reflect the growing popularity of productive,
customizable lisp based systems.

## Philosophy 

Stumpwm is a "everything-and-the-kitchen-sink WM" or "the emacs of
WMs."

**StumpWM:Windows::Emacs:Text**

* StumpWM is
  * Hackable
  * A tileable window manager
  * Includes support for floats
  * Written in Common Lisp
  * Compatible with many lisp distributions
  * A Superior window managing experience 
* StumpWM is *not*
  * Minimalist
  * Narrow Scope
  * Configured by editing the source directly
  * A full blown desktop environment

If you want a minimalist tiling window manager, then StumpWM is *not*
what you're looking for.  The code base is ~15k sloc, the binaries
produced are ~60mb.

StumpWM manages windows the way emacs manages buffers, or the way
screen manages terminals. If you want a flexible, customizable,
hackable desktop experience, look no further.

# Build & Start Stumpwm

## Prerequisites

* sbcl
* quicklisp (for obtaining the following dependencies, not needed if you use your distribution's package manager.)
* clx
* cl-ppcre
* alexandria

The recommended way to install the dependencies is using Quicklisp.
Follow the instructions at http://www.quicklisp.org/ to install it.
In short: 

```
$ curl -O https://beta.quicklisp.org/quicklisp.lisp
$ sbcl --load quicklisp.lisp
```

Then at the REPL:

```lisp
(quicklisp-quickstart:install)
```

Make sure you have added it to your lisp init file using:

```lisp
 (ql:add-to-init-file)
```

Then, in a repl:

```lisp
 (ql:quickload "clx")
 (ql:quickload "cl-ppcre")
 (ql:quickload "alexandria")
```
## Building

Building stumpwm from git requires that you build the configure script:

```
 autoconf
```

If there's already a configure script then just run it.

```
 ./configure
```

Now build it:

```
 make
```

If all goes well, you should have a stumpwm binary now.  You can run
the binary from where it is or install it, along with the .info
documentation, with:

```
 make install
```

Now that you have a binary, call it from your ~/.xinitrc file:

```
 echo /path/to/stumpwm >> ~/.xinitrc
 startx
```

Hopefully that will put you in X running stumpwm! See [StartUp on the
wiki](https://github.com/sabetts/stumpwm/wiki/StartUp) for more
examples.

# Contributing

Pull requests are always welcome! Here are some guidelines to ensure
that your contribution gets merged in a timely manner: 
* Do's 
  * Add your name to the list of AUTHORS with your pull request.  
  * Preserve comments or docstrings explaining what code does, and
    update them if your patch changes them in a significant way
  * Try to follow an "80 column rule." The current code base does not
    follow this all the time, so don't use it as an example
  * [Use lisp idioms](http://people.ace.ed.ac.uk/staff/medward2/class/moz/cm/doc/contrib/lispstyle.html)
  * If you are working on a major change to the internals, keep us
    informed on stumpwm-devel! Also, it will probably help if the
    changes are made and then incrementally applied to the codebase in
    order to avoid introducing show-stopping bugs.
* Do not's
  * Include emacs local variables 
  * Change whitespace 
  * Write lots of code without supporting comments/documentation
  * Delete comments or docstrings (yes this is a duplicate of above!)
  * Export symbols from packages that aren't widely useful (many times
    a little more thought will reveal how to implement your internal
    change without having to export/break encapsulation)
  * Make stylistic changes that suit your coding style/way of thinking 

Our wiki has fallen into disarray/disrepair, but it is shaping up.  If
you aren't a lisp hacker, you can contribute in the form of
documenting and organizing the wiki. There's a lot of information
floating around, if you find it where you didn't expect it, move or
link to it in a more logical place.

# Wishlist 

Fancy yourself a lisp hacker? Here's a wishlist of features for the
StumpWM universe (in no particular order):
* float-splits (ie allow floating windows over tiled ones)
* Float windows within parent applications (specifically dialogs in
  gimp or firefox).
* tab-list showing the contents of the current frame at the side, top,
  or bottom of the frame
* Emacs' iswitchb function implemented in emacs
  * Re-arranging windows between groups
  * Killing windows
  * Marking windows for batch operations
  * Deleting/adding groups
  * Import data from stumpwm to emacs, use an emacs minor mode to
    implement the above features, then export the data back to stumpwm
    and let stumpwm perform the appropriate actions 
* Emacs' completing-read-multiple function
* Dynamic tiling
* Lock Screen (with support for leaving notes, bonus points if emacs
  is involved)
* Wallpapers! (support pulling from remote sources, changing based on
  timers, and other hacky features)
* Shutdown, restart, suspend, and hibernate functions that don't
  require root access
* Revamped, mouse-friendly mode-line. 
  * Support fixed number of chars for window titles
  * Dynamically trim window titles to fit them all on the mode-line
  * Split the mode-line into multiple cells for containing different information
  * Implement widget icons to indicate system status (new mail, low
    battery, network etc)
  * Support raising windows when left-clicked, closing/killing when right-clicked  

# Help

There's a texinfo manual, stumpwm.texi.  The build scripts generate an
info file you can read in emacs or with the `info' program.  The
manual for the latest git version (may be slightly out of date) is
available to read online at: [The Manual](https://stumpwm.github.io/)

And, as in emacs, you can always do "C-t h v,f,k,c,w" for docstrings
of Variable,Functions,Keys,Commands, and Where-is respectively.

For other stuff (tips tricks and examples) visit the [stumpwm wiki](https://github.com/stumpwm/stumpwm/wiki)

There's a #stumpwm channel on irc.freenode.net, too.

Finally, there's our mailing list (click to sign up)
[stumpwm-devel@nongnu.org](https://lists.nongnu.org/mailman/listinfo/stumpwm-devel).
