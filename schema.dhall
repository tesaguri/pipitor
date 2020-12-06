{-
This is free and unencumbered software released into the public domain.

Anyone is free to copy, modify, publish, use, compile, sell, or
distribute this software, either in source code form or as a compiled
binary, for any purpose, commercial or non-commercial, and by any
means.

In jurisdictions that recognize copyright laws, the author or authors
of this software dedicate any and all copyright interest in the
software to the public domain. We make this dedication for the benefit
of the public at large and to the detriment of our heirs and
successors. We intend this dedication to be an overt act of
relinquishment in perpetuity of all present and future rights to this
software under copyright law.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.

For more information, please refer to <http://unlicense.org/>
-}

-- A Dhall type definition for the Pipitor manifest file.
-- The following is a recommended way of importing the file:
-- let Pipitor = https://raw.githubusercontent.com/tesaguri/pipitor/master/schema.dhall sha256:7cda9e784009d372f4b6272be2487647cfa02587faceb2c746b94927c4dec3fc

let Bool/not =
      https://prelude.dhall-lang.org/v20.0.0/Bool/not.dhall sha256:723df402df24377d8a853afed08d9d69a0a6d86e2e5b2bac8960b0d4756c7dc4

let List/concatMap =
      https://prelude.dhall-lang.org/v20.0.0/List/concatMap.dhall sha256:3b2167061d11fda1e4f6de0522cbe83e0d5ac4ef5ddf6bb0b2064470c5d3fb64

let List/map =
      https://prelude.dhall-lang.org/v20.0.0/List/map.dhall sha256:dd845ffb4568d40327f2a817eb42d1c6138b929ca758d50bc33112ef3c885680

let List/null =
      https://prelude.dhall-lang.org/v20.0.0/List/null.dhall sha256:2338e39637e9a50d66ae1482c0ed559bbcc11e9442bfca8f8c176bbcd9c4fc80

let List/unpackOptionals =
      https://prelude.dhall-lang.org/v20.0.0/List/unpackOptionals.dhall sha256:0cbaa920f429cf7fc3907f8a9143203fe948883913560e6e1043223e6b3d05e4

let Optional/null =
      https://prelude.dhall-lang.org/v20.0.0/Optional/null.dhall sha256:3871180b87ecaba8b53fffb2a8b52d3fce98098fab09a6f759358b9e8042eedc

let Duration = { secs : Natural, nanos : Natural }

let Duration/from_secs = λ(secs : Natural) → { secs, nanos = 0 } : Duration

let Duration/from_mins = λ(mins : Natural) → Duration/from_secs (60 * mins)

let Duration/from_hours =
      λ(hours : Natural) → Duration/from_secs (60 * 60 * hours)

let Duration/from_days =
      λ(days : Natural) → Duration/from_secs (24 * 60 * 60 * days)

let Credentials = { identifier : Text, secret : Text }

let Topic = < Feed : Text | Twitter : Natural >

let Filter =
      { Type = { title : Text, text : Optional Text }
      , default.text = None Text
      }

let Outbox = < Twitter : Natural >

let Rule =
      { Type =
          { filter : Optional Filter.Type
          , exclude : Optional Filter.Type
          , outbox : List Outbox
          , topics : List Topic
          }
      , default = { filter = None Filter.Type, exclude = None Filter.Type }
      }

let Rule/feedTopics
    : Rule.Type → List Text
    = λ(rule : Rule.Type) →
        List/unpackOptionals
          Text
          ( List/map
              Topic
              (Optional Text)
              ( λ(topic : Topic) →
                  merge
                    { Feed = λ(uri : Text) → Some uri
                    , Twitter = λ(_ : Natural) → None Text
                    }
                    topic
              )
              rule.topics
          )

let Rule/twitterTopics
    : Rule.Type → List Natural
    = λ(rule : Rule.Type) →
        List/unpackOptionals
          Natural
          ( List/map
              Topic
              (Optional Natural)
              ( λ(topic : Topic) →
                  merge
                    { Twitter = λ(id : Natural) → Some id
                    , Feed = λ(_ : Text) → None Natural
                    }
                    topic
              )
              rule.topics
          )

let Rule/twitterOutboxes
    : Rule.Type → List Natural
    = λ(rule : Rule.Type) →
        List/unpackOptionals
          Natural
          ( List/map
              Outbox
              (Optional Natural)
              ( λ(outbox : Outbox) →
                  merge { Twitter = λ(id : Natural) → Some id } outbox
              )
              rule.outbox
          )

let WebSub =
      { Type =
          { callback : Text, bind : Optional Text, renewal_margin : Duration }
      , default = { bind = None Text, renewal_margin = Duration/from_hours 1 }
      }

let TwitterList =
      { Type = { id : Natural, interval : Duration, delay : Duration }
      , default =
        { interval = Duration/from_secs 1, delay = Duration/from_secs 0 }
      }

let Twitter =
      { Type =
          { client : Credentials
          , user : Natural
          , stream : Bool
          , list : Optional TwitterList.Type
          }
      , default = { stream = False, list = None TwitterList.Type }
      }

let Manifest =
      { Type =
          { database_url : Optional Text
          , websub : Optional WebSub.Type
          , twitter : Optional Twitter.Type
          , rule : List Rule.Type
          , skip_duplicate : Bool
          }
      , default =
        { database_url = None Text
        , websub = None WebSub.Type
        , skip_duplicate = False
        }
      }

let Manifest/feedTopics
    : Manifest.Type → List Text
    = λ(manifest : Manifest.Type) →
        List/concatMap Rule.Type Text Rule/feedTopics manifest.rule

let Manifest/twitterTopics
    : Manifest.Type → List Natural
    = λ(manifest : Manifest.Type) →
        List/concatMap Rule.Type Natural Rule/twitterTopics manifest.rule

let Manifest/twitterOutboxes
    : Manifest.Type → List Natural
    = λ(manifest : Manifest.Type) →
        List/concatMap Rule.Type Natural Rule/twitterOutboxes manifest.rule

let Manifest/validate =
      λ(manifest : Manifest.Type) →
            Bool/not (Optional/null Twitter.Type manifest.twitter)
        ||  List/null
              Natural
              (   Manifest/twitterTopics manifest
                # Manifest/twitterOutboxes manifest
              )

in  { Duration/from_secs
    , Duration/from_mins
    , Duration/from_hours
    , Duration/from_days
    , Duration
    , Topic
    , Filter
    , Outbox
    , Rule
    , Rule/feedTopics
    , Rule/twitterTopics
    , Rule/twitterOutboxes
    , WebSub
    , TwitterList
    , Twitter
    , Manifest
    , Manifest/feedTopics
    , Manifest/twitterTopics
    , Manifest/twitterOutboxes
    , Manifest/validate
    }
