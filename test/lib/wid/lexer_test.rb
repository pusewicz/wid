# frozen_string_literal: true

module Wid
  class LexerTest < Test
    def test_number
      assert_equal([
        build_token(:NUMBER, "1"),
        build_token(:NUMBER, "2"),
        build_token(:NUMBER, "3")
      ], Lexer.tokenize("1 2 3"))

      assert_equal([
        build_token(:NUMBER, "1.3"),
        build_token(:NUMBER, "2.7"),
        build_token(:NUMBER, "3.88999")
      ], Lexer.tokenize("1.3 2.7 3.88999"))
    end

    def test_whitespace_skip
      tokens = Lexer.tokenize("    1    ")

      assert_equal([
        build_token(:NUMBER, "1")
      ], tokens)
    end

    def test_comment_skip
      tokens = Lexer.tokenize("# This is a comment\n1")

      assert_equal([
        build_token(:NUMBER, "1")
      ], tokens)
    end

    def test_comment_after_expression
      tokens = Lexer.tokenize("1 # This is a comment")

      assert_equal([
        build_token(:NUMBER, "1")
      ], tokens)
    end

    def test_string
      assert_equal([
        build_token(:STRING, "\"foo\""),
        build_token(:STRING, "'bar'")
      ], Lexer.tokenize("\"foo\" 'bar'"))
    end

    def test_new_line
      tokens = Lexer.tokenize("1\n3")

      assert_equal([
        build_token(:NUMBER, "1"),
        build_token(:"\n", "\n"),
        build_token(:NUMBER, "3")
      ], tokens)

      assert_equal(0, tokens[0].line)
      assert_equal(0, tokens[1].line)
      assert_equal(1, tokens[2].line)
    end

    def test_column
      tokens = Lexer.tokenize("1 + 2\n       3  + 17\nputs(foo)    \n")

      assert_equal(0, tokens[0].column) # 1
      assert_equal(2, tokens[1].column) # +
      assert_equal(4, tokens[2].column) # 2
      assert_equal(5, tokens[3].column) # \n
      assert_equal(7, tokens[4].column) # 3
      assert_equal(10, tokens[5].column) # +
      assert_equal(12, tokens[6].column) # 17
      assert_equal(14, tokens[7].column) # \n
      assert_equal(0, tokens[8].column) # puts
      assert_equal(4, tokens[9].column) # (
      assert_equal(5, tokens[10].column) # foo
      assert_equal(8, tokens[11].column) # )
      assert_equal(13, tokens[12].column) # \n
    end

    def test_identifier
      tokens = Lexer.tokenize("foo")

      assert_equal([
        build_token(:IDENTIFIER, "foo")
      ], tokens)
    end

    def test_keywords
      tokens = Lexer.tokenize("true false nil do end")

      assert_equal([
        build_token(:true, "true"),
        build_token(:false, "false"),
        build_token(:nil, "nil"),
        build_token(:do, "do"),
        build_token(:end, "end")
      ], tokens)
    end

    def test_symbols
      tokens = Lexer.tokenize("{ } ( ) [ ] . , ;")

      assert_equal([
        build_token(:"{", "{"),
        build_token(:"}", "}"),
        build_token(:"(", "("),
        build_token(:")", ")"),
        build_token(:"[", "["),
        build_token(:"]", "]"),
        build_token(:".", "."),
        build_token(:",", ","),
        build_token(:";", ";")
      ], tokens)
    end

    def test_var_assignment
      tokens = Lexer.tokenize("foo = 1")

      assert_equal([
        build_token(:IDENTIFIER, "foo"),
        build_token(:SIMPLE_ASSIGN, "="),
        build_token(:NUMBER, "1")
      ], tokens)
    end

    def test_nil
      tokens = Lexer.tokenize("nil")

      assert_equal([
        build_token(:nil, "nil")
      ], tokens)
    end

    def test_puts_method_call
      tokens = Lexer.tokenize('puts("Hello, World!")')

      assert_equal([
        build_token(:IDENTIFIER, "puts"),
        build_token(:"(", "("),
        build_token(:STRING, "\"Hello, World!\""),
        build_token(:")", ")")
      ], tokens)

      assert_equal("puts", tokens[0].value)
      assert_equal("\"Hello, World!\"", tokens[2].value)
    end

    def test_invalid_token
      assert_raises(Lexer::SyntaxError) do
        Lexer.tokenize("foo @ bar")
      end
    end

    private

    def build_token(type, value)
      Lexer::Token.new(type: type, value: value, line: nil, column: nil)
    end
  end
end
