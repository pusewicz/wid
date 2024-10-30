module Wid
  module AST
    class Node
      class Program < Node
        def initialize(statements:)
          @statements = statements
        end

        def to_h
          super.merge(statements: @statements.map(&:to_h))
        end
      end
    end
  end
end
