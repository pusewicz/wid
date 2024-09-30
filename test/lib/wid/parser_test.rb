module Wid
  class ParserTest < Test
    def test_parse
      tokens = Lexer.tokenize(<<~WID)
        1 + 2.0
      WID

      ast = Parser.parse(tokens)

      fail "TODO: Implement ParserTest"
    end
  end
end
