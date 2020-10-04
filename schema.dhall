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
-- let Pipitor = https://raw.githubusercontent.com/tesaguri/pipitor/master/schema.dhall sha256:b5747fd58250bb92a6450310e58641fd79385095e0fc10a4686590f4cf37cf72

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

let TwitterList =
      { Type = { id : Natural, delay : Natural }, default.delay = 0 }

let Twitter =
      { Type =
          { user : Natural, stream : Bool, list : Optional TwitterList.Type }
      , default = { stream = False, list = None TwitterList.Type }
      }

let Manifest =
      { Type =
          { credentials : Optional Text
          , database_url : Optional Text
          , twitter : Twitter.Type
          , rule : List Rule.Type
          , skip_duplicate : Bool
          }
      , default =
        { credentials = None Text
        , database_url = None Text
        , skip_duplicate = False
        }
      }

in  { Topic, Filter, Outbox, Rule, TwitterList, Twitter, Manifest }
